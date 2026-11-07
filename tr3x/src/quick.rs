//! `tr3x new --prompt …`: a thread launched without the composer, for Shortcuts, Siri, Spotlight,
//! a Finder Quick Action or a script. Apple Shortcuts can't hand a Rex action any input, but its
//! Run Command can run this on a host, with what you said or typed. The thread gets T3's
//! defaults for the project, as the composer starts with, and its tab opens on your Rex apps
//! once sync makes it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use t3client::env::Env;
use t3client::launch::{LaunchRequest, Workspace};
use t3client::models;
use t3client::projection::ShellState;

use crate::t3;

pub struct Ask {
    pub prompt: String,
    /// The machine, by name or id; else this Mac's T3, else the first paired machine that answers.
    pub env: Option<String>,
    /// A project by name or id.
    pub project: Option<String>,
    /// A folder: the project whose folder holds it, made one when there is none (on this Mac).
    pub folder: Option<PathBuf>,
    /// Go to the thread's tab on your Rex apps once sync makes it.
    pub open: bool,
}

pub async fn run(ask: Ask) -> Result<()> {
    if ask.prompt.trim().is_empty() {
        bail!("say what the thread should do: tr3x new --prompt '…'");
    }
    let (client, config, shell) = machine(ask.env.as_deref()).await?;
    let project = match (&ask.project, &ask.folder) {
        (Some(name), _) => by_name(&shell, name)
            .with_context(|| format!("no project called {name:?} on {}", client.name()))?,
        (None, Some(folder)) => match by_folder(&shell, folder) {
            Some(p) => p,
            None if client.is_local() => add(&client, folder).await?,
            None => bail!(
                "{} has no project at {}; folders are added on this Mac only",
                client.name(),
                folder.display()
            ),
        },
        (None, None) => crate::new::projects_by_recent(&shell)
            .0
            .into_iter()
            .next()
            .with_context(|| format!("no projects on {} yet; pass --folder", client.name()))?,
    };
    let project_id = project["id"].as_str().unwrap_or_default().to_string();
    let recent = crate::new::projects_by_recent(&shell).1;
    let settings = crate::new::defaults(&config, &project, recent.get(&project_id));
    let (plan, _) = models::plan_new_thread(&config, &project_id, &settings.choice())?;
    let workspace = match crate::new::starts_in_worktree(&config, &project) {
        true => match base(&client, &project).await {
            Some(base_ref) => Workspace::Worktree {
                base_ref,
                branch: None,
                start_from_origin: false,
            },
            None => Workspace::Root,
        },
        false => Workspace::Root,
    };
    let request = LaunchRequest::new(&client, &project_id, &ask.prompt, None, &plan, workspace)?;
    let receipt = client.launch_thread(&request).await?;
    let key = format!("{}/{}", client.id(), receipt.thread_id);
    println!(
        "Started a thread in {} on {}",
        project["title"].as_str().unwrap_or("the project"),
        client.name()
    );
    if ask.open
        && let Err(e) = crate::find::go_to(&key, None).await
    {
        eprintln!("tr3x: {e:#}");
    }
    Ok(())
}

/// The machine asked for, else this Mac's T3, else the first paired machine that answers.
async fn machine(env: Option<&str>) -> Result<(Arc<Env>, Value, ShellState)> {
    let open = |client: Arc<Env>| async move {
        let (config, shell) = tokio::try_join!(client.server_config(), client.shell())?;
        anyhow::Ok((client, config, shell))
    };
    if let Some(env) = env {
        return open(t3::machine(env)?).await;
    }
    let first = match t3::machine("") {
        Ok(client) => open(client).await,
        Err(e) => Err(e),
    };
    if first.is_ok() {
        return first;
    }
    for other in t3::hub().envs().iter().filter(|m| !m.is_local()) {
        if let Ok(Ok(found)) =
            tokio::time::timeout(Duration::from_secs(5), open(other.clone())).await
        {
            return Ok(found);
        }
    }
    first
}

fn by_name(shell: &ShellState, name: &str) -> Option<Value> {
    shell
        .projects
        .iter()
        .find(|p| {
            p["id"].as_str() == Some(name)
                || p["title"]
                    .as_str()
                    .is_some_and(|t| t.eq_ignore_ascii_case(name))
        })
        .cloned()
}

/// The project whose folder holds `folder`: the deepest one.
fn by_folder(shell: &ShellState, folder: &Path) -> Option<Value> {
    let folder = std::fs::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf());
    shell
        .projects
        .iter()
        .filter_map(|p| {
            let root = PathBuf::from(p["workspaceRoot"].as_str()?);
            let root = std::fs::canonicalize(&root).unwrap_or(root);
            folder
                .starts_with(&root)
                .then(|| (root.components().count(), p))
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, p)| p.clone())
}

/// Makes `folder` a project, through the call `t3 project add` makes.
async fn add(client: &Env, folder: &Path) -> Result<Value> {
    let root = std::fs::canonicalize(folder)
        .with_context(|| format!("{} isn't a folder", folder.display()))?;
    if !root.is_dir() {
        bail!("{} isn't a folder", root.display());
    }
    let root = root.to_string_lossy().into_owned();
    let title = Path::new(&root)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".into());
    let id = uuid::Uuid::new_v4().to_string();
    let rpc = client.rpc().await?;
    let made = rpc
        .call(
            "projects.mutate",
            json!({
                "type": "project.create",
                "commandId": format!("tr3x:{}", uuid::Uuid::new_v4()),
                "projectId": id,
                "title": title,
                "workspaceRoot": root,
            }),
            Duration::from_secs(15),
        )
        .await?;
    Ok(if made["id"].is_string() {
        made
    } else {
        json!({"id": id, "title": title, "workspaceRoot": root})
    })
}

/// The branch a worktree starts from, as the composer picks it: the default, else the current.
async fn base(client: &Env, project: &Value) -> Option<String> {
    let root = project["workspaceRoot"].as_str()?;
    let rpc = client.rpc().await.ok()?;
    let found = rpc
        .call(
            "vcs.listRefs",
            json!({"cwd": root, "limit": 100, "refKind": "local"}),
            Duration::from_secs(15),
        )
        .await
        .ok()?;
    let refs = found["refs"].as_array()?;
    refs.iter()
        .find(|r| r["isDefault"] == true)
        .or_else(|| refs.iter().find(|r| r["current"] == true))
        .or(refs.first())
        .and_then(|r| r["name"].as_str())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(projects: Value) -> ShellState {
        ShellState {
            projects: projects.as_array().cloned().unwrap_or_default(),
            ..Default::default()
        }
    }

    #[test]
    fn a_folder_finds_the_deepest_project_that_holds_it() {
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path().join("code");
        let inner = outer.join("app");
        std::fs::create_dir_all(inner.join("src")).unwrap();
        let shell = shell(json!([
            {"id": "outer", "title": "code", "workspaceRoot": outer.to_string_lossy()},
            {"id": "inner", "title": "app", "workspaceRoot": inner.to_string_lossy()},
        ]));
        let found = |p: &Path| by_folder(&shell, p).map(|v| v["id"].as_str().unwrap().to_string());
        assert_eq!(found(&inner.join("src")).as_deref(), Some("inner"));
        assert_eq!(found(&outer).as_deref(), Some("outer"));
        assert_eq!(found(dir.path()), None);
    }

    #[test]
    fn a_project_is_named_by_title_or_id() {
        let shell = shell(json!([{"id": "p1", "title": "tr3x"}, {"id": "p2", "title": "Site"}]));
        assert_eq!(by_name(&shell, "site").unwrap()["id"], "p2");
        assert_eq!(by_name(&shell, "p1").unwrap()["id"], "p1");
        assert!(by_name(&shell, "nope").is_none());
    }
}
