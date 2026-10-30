//! The pieces the new-thread composer and the model picker share: a row of chips, like the
//! ones under T3's composer, and the menu a chip opens, filtered by typing. Both draw into
//! plain lines; icons come from `icons`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;
use t3term::models;

use crate::chat::{BOLD, DIM, RESET, fit};
use crate::icons::{Icon, Painter};

pub const INVERSE: &str = "\x1b[7m";

#[derive(Clone, Debug)]
pub struct Item {
    pub id: String,
    pub label: String,
    /// A dim note after the label, such as a branch's remote.
    pub detail: String,
    pub icon: Option<Icon>,
    /// Items with a section print under its heading, in the order they come.
    pub section: Option<String>,
}

impl Item {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Item {
        Item {
            id: id.into(),
            label: label.into(),
            detail: String::new(),
            icon: None,
            section: None,
        }
    }
}

pub enum MenuKey {
    Chose(String),
    Close,
    Moved,
}

#[derive(Default)]
pub struct Menu {
    pub items: Vec<Item>,
    pub filter: String,
    /// Index into `shown()`.
    pub selected: usize,
    /// The id of the current value, marked in the list.
    pub current: Option<String>,
    /// Screen rows of the items drawn last, for clicks: (row, index into items).
    rows: Vec<(u16, usize)>,
}

impl Menu {
    pub fn new(items: Vec<Item>, current: Option<&str>) -> Menu {
        let mut menu = Menu {
            items,
            current: current.map(str::to_string),
            ..Menu::default()
        };
        if let Some(at) = menu
            .shown()
            .iter()
            .position(|&i| Some(menu.items[i].id.as_str()) == current)
        {
            menu.selected = at;
        }
        menu
    }

    /// Indices of the items the filter keeps, in order.
    pub fn shown(&self) -> Vec<usize> {
        let words: Vec<String> = self
            .filter
            .to_lowercase()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        (0..self.items.len())
            .filter(|&i| {
                let item = &self.items[i];
                let hay = format!(
                    "{} {} {}",
                    item.label,
                    item.detail,
                    item.section.as_deref().unwrap_or("")
                )
                .to_lowercase();
                words.iter().all(|w| hay.contains(w))
            })
            .collect()
    }

    pub fn key(&mut self, key: KeyEvent) -> MenuKey {
        let shown = self.shown();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return MenuKey::Close,
            KeyCode::Enter | KeyCode::Tab => {
                return match shown.get(self.selected) {
                    Some(&i) => MenuKey::Chose(self.items[i].id.clone()),
                    None => MenuKey::Close,
                };
            }
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Char('p') if ctrl => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(shown.len().saturating_sub(1)),
            KeyCode::Char('n') if ctrl => {
                self.selected = (self.selected + 1).min(shown.len().saturating_sub(1))
            }
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(8),
            KeyCode::PageDown => {
                self.selected = (self.selected + 8).min(shown.len().saturating_sub(1))
            }
            KeyCode::Backspace => {
                if self.filter.pop().is_none() {
                    return MenuKey::Close;
                }
                self.selected = 0;
            }
            KeyCode::Char(c) if !ctrl => {
                self.filter.push(c);
                self.selected = 0;
            }
            _ => {}
        }
        MenuKey::Moved
    }

    /// The item drawn on screen row `row`, from the last draw.
    pub fn click(&self, row: u16) -> Option<String> {
        self.rows
            .iter()
            .find(|(r, _)| *r == row)
            .map(|(_, i)| self.items[*i].id.clone())
    }

    /// The menu as at most `height` lines, starting on screen row `top` (0-based), `width`
    /// columns wide, scrolled to keep the selection in view.
    pub fn lines(&mut self, painter: &mut Painter, top: u16, width: usize, height: usize) -> Vec<String> {
        let shown = self.shown();
        self.selected = self.selected.min(shown.len().saturating_sub(1));
        // Each item with its heading when the section changes.
        let mut rows: Vec<(Option<usize>, String)> = Vec::new();
        if !self.filter.is_empty() {
            rows.push((None, format!("{DIM}/{RESET}{}", self.filter)));
        }
        let mut section: Option<&str> = None;
        let mut selected_row = 0;
        for (n, &i) in shown.iter().enumerate() {
            let item = &self.items[i];
            if item.section.as_deref() != section {
                section = item.section.as_deref();
                if let Some(s) = section {
                    rows.push((None, format!("{DIM}{}{RESET}", fit(s, width))));
                }
            }
            if n == self.selected {
                selected_row = rows.len();
            }
            rows.push((Some(i), String::new()));
        }
        if shown.is_empty() {
            rows.push((None, format!("{DIM}No matches{RESET}")));
        }
        let start = (selected_row + 1).saturating_sub(height);
        self.rows.clear();
        let mut out = Vec::new();
        for (k, (item, text)) in rows.into_iter().skip(start).take(height).enumerate() {
            let Some(i) = item else {
                out.push(text);
                continue;
            };
            self.rows.push((top + k as u16, i));
            let item = &self.items[i];
            let on = shown.get(self.selected) == Some(&i);
            let mark = if self.current.as_deref() == Some(item.id.as_str()) {
                "✓"
            } else {
                " "
            };
            let icon = item.icon.as_ref().map(|icon| painter.draw(icon) + " ");
            let used = 2 + if icon.is_some() { 3 } else { 0 };
            let room = width.saturating_sub(used + 1);
            let label = fit(&item.label, room);
            let detail_room = room.saturating_sub(unicode_width::UnicodeWidthStr::width(label.as_str()) + 2);
            let detail = if item.detail.is_empty() || detail_room < 4 {
                String::new()
            } else {
                format!("  {DIM}{}{RESET}", fit(&item.detail, detail_room))
            };
            let label = if on {
                format!("{INVERSE}{BOLD} {label} {RESET}")
            } else {
                format!(" {label} ")
            };
            out.push(format!("{mark} {}{label}{detail}", icon.unwrap_or_default()));
        }
        out
    }
}

/// A chip: an optional icon and a label, drawn as `icon label ▾`.
pub struct Chip {
    pub icon: Option<Icon>,
    pub label: String,
}

/// Draws chips on one line with two spaces between them. Returns the line and each chip's
/// columns, 1-based and inclusive, for clicks.
pub fn chips(
    painter: &mut Painter,
    chips: &[Chip],
    focused: Option<usize>,
    open: Option<usize>,
    left: usize,
) -> (String, Vec<(usize, usize)>) {
    let mut line = String::new();
    let mut spans = Vec::new();
    let mut col = left + 1;
    for (i, chip) in chips.iter().enumerate() {
        if i > 0 {
            line.push_str("  ");
            col += 2;
        }
        let start = col;
        let label = format!("{} ▾", chip.label);
        let width = unicode_width::UnicodeWidthStr::width(label.as_str())
            + if chip.icon.is_some() { 3 } else { 0 };
        let style = if open == Some(i) || focused == Some(i) {
            INVERSE
        } else {
            DIM
        };
        let icon = chip
            .icon
            .as_ref()
            .map(|icon| painter.draw(icon) + " ")
            .unwrap_or_default();
        line.push_str(&format!("{icon}{style}{label}{RESET}"));
        col += width;
        spans.push((start, col - 1));
    }
    (line, spans)
}

/// The model, effort and access a thread or a new thread goes out with.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Settings {
    pub instance: String,
    pub model: String,
    pub effort: Option<String>,
    pub mode: String,
}

impl Settings {
    /// A thread's current settings, as the Reply header shows them.
    pub fn of_thread(config: &Value, thread: &Value) -> Settings {
        let s = models::thread_settings(config, thread);
        let mut out = Settings {
            instance: s["instanceId"].as_str().unwrap_or_default().to_string(),
            model: s["model"].as_str().unwrap_or_default().to_string(),
            effort: None,
            mode: s["runtimeMode"].as_str().unwrap_or("full-access").to_string(),
        };
        if let Some(id) = out.effort_descriptor(config).and_then(|d| d["id"].as_str()) {
            out.effort = s["options"][id].as_str().map(str::to_string);
        }
        out
    }

    pub fn provider<'a>(&self, config: &'a Value) -> Option<&'a Value> {
        models::providers(config)
            .iter()
            .find(|p| p["instanceId"].as_str() == Some(self.instance.as_str()))
    }

    pub fn model_entry<'a>(&self, config: &'a Value) -> Option<&'a Value> {
        models::models(self.provider(config)?)
            .iter()
            .find(|m| m["slug"].as_str() == Some(self.model.as_str()))
    }

    fn effort_descriptor<'a>(&self, config: &'a Value) -> Option<&'a Value> {
        models::effort_descriptor(self.model_entry(config)?)
    }

    pub fn model_label(&self, config: &Value) -> String {
        self.model_entry(config)
            .and_then(|m| m["name"].as_str())
            .unwrap_or(&self.model)
            .to_string()
    }

    pub fn effort_label(&self, config: &Value) -> Option<String> {
        let d = self.effort_descriptor(config)?;
        let id = self.effort.clone().or_else(|| default_option(d))?;
        Some(
            d["options"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|o| o["id"].as_str() == Some(id.as_str()))
                .and_then(|o| o["label"].as_str())
                .unwrap_or(&id)
                .to_string(),
        )
    }

    pub fn mode_label(&self) -> String {
        models::runtime_mode_label(&self.mode).to_string()
    }

    /// Switches to `instance/slug`, keeping the effort when the new model offers it.
    pub fn set_model(&mut self, config: &Value, id: &str) {
        let Some((instance, slug)) = id.split_once('/') else {
            return;
        };
        self.instance = instance.to_string();
        self.model = slug.to_string();
        let offered = self.effort_descriptor(config).is_some_and(|d| {
            d["options"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|o| o["id"].as_str() == self.effort.as_deref())
        });
        if !offered {
            self.effort = None;
        }
        if !self.modes(config).iter().any(|(m, _)| *m == self.mode)
            && let Some((m, _)) = self.modes(config).first()
        {
            self.mode = m.to_string();
        }
    }

    /// The runtime modes the provider supports, with their labels.
    pub fn modes(&self, config: &Value) -> Vec<(&'static str, &'static str)> {
        let listed: Option<Vec<&str>> = self
            .provider(config)
            .and_then(|p| p["supportedRuntimeModes"].as_array())
            .map(|a| a.iter().filter_map(Value::as_str).collect());
        models::RUNTIME_MODES
            .iter()
            .filter(|(m, _)| listed.as_ref().is_none_or(|l| l.contains(m)))
            .copied()
            .collect()
    }

    /// What `models::plan` and `models::plan_new_thread` take.
    pub fn choice(&self) -> models::Choice {
        models::Choice {
            model: Some(format!("{}/{}", self.instance, self.model)),
            effort: self.effort.clone(),
            runtime_mode: Some(self.mode.clone()),
            ..models::Choice::default()
        }
    }
}

fn default_option(descriptor: &Value) -> Option<String> {
    let options = descriptor["options"].as_array()?;
    options
        .iter()
        .find(|o| o["isDefault"] == true)
        .or(options.first())
        .and_then(|o| o["id"].as_str())
        .map(str::to_string)
}

/// Every model of every enabled provider, under the provider's name, with its logo.
pub fn model_items(config: &Value, logos: &std::collections::HashMap<String, Icon>) -> Vec<Item> {
    let mut items = Vec::new();
    for provider in models::providers(config).iter().filter(|p| models::enabled(p)) {
        let name = provider["displayName"]
            .as_str()
            .or(provider["instanceId"].as_str())
            .unwrap_or("Provider");
        let logo = provider["instanceId"]
            .as_str()
            .and_then(|i| logos.get(i))
            .cloned();
        for model in models::models(provider) {
            let mut item = Item::new(
                models::qualified(provider, model),
                model["name"].as_str().or(model["slug"].as_str()).unwrap_or("Model"),
            );
            item.section = Some(name.to_string());
            item.icon = logo.clone();
            items.push(item);
        }
    }
    items
}

pub fn effort_items(config: &Value, settings: &Settings) -> Vec<Item> {
    settings
        .effort_descriptor(config)
        .and_then(|d| d["options"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|o| {
            let id = o["id"].as_str()?;
            Some(Item::new(id, o["label"].as_str().unwrap_or(id)))
        })
        .collect()
}

pub fn mode_items(config: &Value, settings: &Settings) -> Vec<Item> {
    settings
        .modes(config)
        .into_iter()
        .map(|(id, label)| Item::new(id, label))
        .collect()
}

/// The current effort's id, for marking it in the menu.
pub fn effort_now(config: &Value, settings: &Settings) -> Option<String> {
    settings
        .effort
        .clone()
        .or_else(|| default_option(settings.effort_descriptor(config)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventState;
    use serde_json::json;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: crossterm::event::KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn typing_filters_and_enter_chooses() {
        let mut menu = Menu::new(
            vec![Item::new("a", "Opus 5.5"), Item::new("b", "Haiku 5.5"), Item::new("c", "GPT-6")],
            Some("c"),
        );
        assert_eq!(menu.selected, 2);
        for c in "hai".chars() {
            menu.key(key(KeyCode::Char(c)));
        }
        assert_eq!(menu.shown(), vec![1]);
        assert!(matches!(menu.key(key(KeyCode::Enter)), MenuKey::Chose(id) if id == "b"));
    }

    #[test]
    fn a_new_model_keeps_an_effort_it_offers() {
        let config = json!({"providers": [{"instanceId": "c", "enabled": true, "models": [
            {"slug": "x", "name": "X", "capabilities": {"optionDescriptors": [{"id": "effort", "type": "select",
                "options": [{"id": "low"}, {"id": "high", "isDefault": true}]}]}},
            {"slug": "y", "name": "Y", "capabilities": {"optionDescriptors": [{"id": "effort", "type": "select",
                "options": [{"id": "medium", "isDefault": true}]}]}}
        ]}]});
        let mut s = Settings { instance: "c".into(), model: "x".into(), effort: Some("low".into()), mode: "full-access".into() };
        s.set_model(&config, "c/y");
        assert_eq!(s.effort, None);
        assert_eq!(s.effort_label(&config).as_deref(), Some("medium"));
        assert_eq!(s.choice().model.as_deref(), Some("c/y"));
    }
}
