//! A model picked while the thread works. Changing the thread's model mid-run can restart its
//! agent, so T3's composer holds the choice and sends it with the next message; this is that
//! hold, shared by the picker's layer and the Reply pane. Keyed by `environment/thread`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use t3client::models::{self, Choice, Plan};

use crate::picker::Settings;

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Draft {
    /// `instance/slug`.
    pub model: String,
    pub effort: Option<String>,
}

impl Draft {
    pub fn of(settings: &Settings) -> Draft {
        Draft {
            model: format!("{}/{}", settings.instance, settings.model),
            effort: settings.effort.clone(),
        }
    }

    /// `settings` with this model and effort.
    pub fn over(&self, config: &Value, settings: &mut Settings) {
        settings.set_model(config, &self.model);
        if self.effort.is_some() {
            settings.effort = self.effort.clone();
        }
    }

    /// The plan that sends this model with a message.
    pub fn plan(&self, config: &Value, thread: &Value) -> anyhow::Result<Plan> {
        let choice = Choice {
            model: Some(self.model.clone()),
            effort: self.effort.clone(),
            ..Choice::default()
        };
        models::plan(config, thread, &choice)
    }
}

fn path(thread: &str) -> PathBuf {
    let name: String = thread
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    crate::state::dir()
        .join("drafts")
        .join(format!("{name}.json"))
}

pub fn load(thread: &str) -> Option<Draft> {
    serde_json::from_slice(&std::fs::read(path(thread)).ok()?).ok()
}

pub fn save(thread: &str, draft: &Draft) -> anyhow::Result<()> {
    let path = path(thread);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_vec(draft)?)?;
    Ok(())
}

pub fn clear(thread: &str) {
    let _ = std::fs::remove_file(path(thread));
}
