//! `tr3x surface`: what a T3 build offers, against `t3-surface.json`, the surface tr3x was
//! last checked against. CI runs it on each nightly; see `.github/workflows/surface.yml`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use t3client::surface::{Change, Surface};

pub fn run(
    from: Option<PathBuf>,
    label: Option<String>,
    check: Option<PathBuf>,
    write: Option<PathBuf>,
) -> Result<()> {
    let from = match from {
        Some(path) => path,
        None => t3client::surface::installed()
            .into_iter()
            .next()
            .context("no T3 build on this Mac; name one with --from")?,
    };
    let mut surface = Surface::from_file(&from)?;
    surface.t3 = label;
    if let Some(path) = &check {
        let known = load(path)?;
        let changes = known.diff(&surface);
        if changes.is_empty() {
            println!("No change from {}", path.display());
            return Ok(());
        }
        print!("{}", report(&changes));
        if write.is_none() {
            std::process::exit(1);
        }
    }
    let json = serde_json::to_string_pretty(&surface)? + "\n";
    match write {
        Some(path) => std::fs::write(&path, json)
            .with_context(|| format!("can't write {}", path.display()))?,
        None if check.is_none() => print!("{json}"),
        None => {}
    }
    Ok(())
}

fn load(path: &Path) -> Result<Surface> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("can't read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("can't read {}", path.display()))
}

/// The changes as Markdown, for a pull request: added first, since that's what to build next.
fn report(changes: &[Change]) -> String {
    let mut out = String::new();
    for (added, heading) in [(true, "Added"), (false, "Removed")] {
        let these: Vec<&Change> = changes.iter().filter(|c| c.added == added).collect();
        if these.is_empty() {
            continue;
        }
        out += &format!("### {heading}\n\n");
        let mut list = "";
        for c in these {
            if c.list != list {
                out += &format!(
                    "{}**{}**\n",
                    if list.is_empty() { "" } else { "\n" },
                    c.list
                );
                list = c.list;
            }
            out += &format!("- `{}`\n", c.name);
        }
        out += "\n";
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::bail;
    use std::collections::BTreeSet;

    /// Fails when `names` has something `surface` lacks, naming it.
    pub fn require(surface: &Surface, kind: &str, names: &[String]) -> Result<()> {
        let list = match kind {
            "commands" => &surface.commands,
            "methods" => &surface.methods,
            "capabilities" => &surface.capabilities,
            _ => bail!("no list {kind}"),
        };
        let missing: Vec<&String> = names.iter().filter(|n| !list.contains(*n)).collect();
        if missing.is_empty() {
            Ok(())
        } else {
            bail!("tr3x uses {kind} T3 no longer has: {missing:?}")
        }
    }

    fn known() -> Surface {
        serde_json::from_str(include_str!("../t3-surface.json")).unwrap()
    }

    /// Every string literal in tr3x's and t3client's sources that follows `before`.
    fn literals(before: &str) -> BTreeSet<String> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut out = BTreeSet::new();
        for dir in ["tr3x/src", "t3client/src"] {
            let mut stack = vec![root.join(dir)];
            while let Some(dir) = stack.pop() {
                for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                        continue;
                    }
                    if path.extension().is_none_or(|e| e != "rs") {
                        continue;
                    }
                    let text = std::fs::read_to_string(&path).unwrap();
                    // Tests make up commands and methods of their own.
                    let text = text.split("#[cfg(test)]").next().unwrap_or_default();
                    for piece in text.split(before).skip(1) {
                        if let Some(name) = piece.split('"').next() {
                            out.insert(name.to_string());
                        }
                    }
                }
            }
        }
        out
    }

    /// What tr3x sends T3 is in the surface it was checked against, so a nightly that drops
    /// one fails here, in the pull request that brings that nightly's surface in.
    #[test]
    fn tr3x_only_uses_what_t3_has() {
        let surface = known();
        let commands: Vec<String> = literals("\"type\": \"")
            .into_iter()
            .filter(|t| t.contains('.'))
            .collect();
        assert!(commands.contains(&"thread.settle".to_string()));
        require(&surface, "commands", &commands).unwrap();
        let methods: Vec<String> = ["call(\"", "call(\n                \"", "\"orchestration."]
            .iter()
            .flat_map(|b| literals(b))
            .map(|m| {
                if m.contains('.') {
                    m
                } else {
                    format!("orchestration.{m}")
                }
            })
            // Rex's own methods, which `Rex::call` sends.
            .filter(|m| {
                !["block.", "session.", "client.", "server."]
                    .iter()
                    .any(|rex| m.starts_with(rex))
            })
            .collect();
        assert!(methods.contains(&"orchestration.subscribeShell".to_string()));
        require(&surface, "methods", &methods).unwrap();
        let caps: Vec<String> = literals("has_capability(\"").into_iter().collect();
        require(&surface, "capabilities", &caps).unwrap();
    }

    #[test]
    fn every_action_needs_a_capability_t3_has() {
        let surface = known();
        let caps: Vec<String> = crate::actions::ALL
            .iter()
            .filter_map(|a| a.needs.map(str::to_string))
            .collect();
        require(&surface, "capabilities", &caps).unwrap();
    }

    #[test]
    fn reports_added_first() {
        let changes = [
            Change {
                list: "commands",
                name: "thread.gone".into(),
                added: false,
            },
            Change {
                list: "commands",
                name: "thread.fork".into(),
                added: true,
            },
            Change {
                list: "methods",
                name: "orchestration.forceStopThread".into(),
                added: true,
            },
        ];
        assert_eq!(
            report(&changes),
            "### Added\n\n**commands**\n- `thread.fork`\n\n**methods**\n- `orchestration.forceStopThread`\n\n### Removed\n\n**commands**\n- `thread.gone`\n\n"
        );
    }
}
