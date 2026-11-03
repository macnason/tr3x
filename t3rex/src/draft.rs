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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// What the picker holds while a thread works, Reply reads back and shows and sends.
    #[test]
    fn a_held_choice_comes_back_as_the_next_messages_settings() {
        let effort = |options: Value| json!({"optionDescriptors": [{"id": "effort", "type": "select", "options": options}]});
        let config = json!({"providers": [{"instanceId": "c", "enabled": true, "models": [
            {"slug": "x", "name": "X", "capabilities": effort(json!([{"id": "low"}, {"id": "high"}]))},
            {"slug": "y", "name": "Y", "capabilities": effort(json!([{"id": "low"}, {"id": "max"}]))}]}]});
        let running = Settings {
            instance: "c".into(),
            model: "x".into(),
            effort: Some("high".into()),
            mode: "auto".into(),
        };
        // Picked in the picker: a model, then one of its efforts.
        let mut next = running.clone();
        next.set_model(&config, "c/y");
        next.effort = Some("max".into());
        let held = Draft::of(&next);
        assert_eq!(held.model, "c/y");
        let held: Draft = serde_json::from_slice(&serde_json::to_vec(&held).unwrap()).unwrap();

        let mut shown = running.clone();
        held.over(&config, &mut shown);
        assert_eq!(
            shown,
            Settings {
                model: "y".into(),
                effort: Some("max".into()),
                ..running.clone()
            }
        );

        // A model alone keeps no effort the new model can't run.
        let mut only_model = running.clone();
        only_model.set_model(&config, "c/y");
        let mut shown = running;
        Draft::of(&only_model).over(&config, &mut shown);
        assert_eq!((shown.model.as_str(), shown.effort), ("y", None));
    }
}
