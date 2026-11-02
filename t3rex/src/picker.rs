//! The pieces the new-thread composer and the model picker share: a row of chips, like the
//! ones under T3's composer, and the menu a chip opens, filtered by typing. Both draw into
//! plain lines; icons come from `icons`.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;
use t3client::models;

use crate::chat::{BOLD, DIM, RESET, fit};
use crate::icons::{Icon, Painter};
use crate::pick::Field;

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
    /// Each word of the filter matches its letters in order, not only as one piece.
    pub fuzzy: bool,
    /// Details sit at the right edge, in a column of their own.
    pub details_right: bool,
    /// The filter is drawn elsewhere, such as in an input above the menu.
    pub hide_filter: bool,
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
                words
                    .iter()
                    .all(|w| hay.contains(w) || self.fuzzy && in_order(w, &hay))
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
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::SUPER) => {
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
    pub fn lines(
        &mut self,
        painter: &mut Painter,
        top: u16,
        width: usize,
        height: usize,
    ) -> Vec<String> {
        let shown = self.shown();
        self.selected = self.selected.min(shown.len().saturating_sub(1));
        // Each item with its heading when the section changes.
        let mut rows: Vec<(Option<usize>, String)> = Vec::new();
        if !self.filter.is_empty() && !self.hide_filter {
            rows.push((None, format!("{DIM}/{RESET}{}", self.filter)));
        }
        let mut section: Option<&str> = None;
        let mut selected_row = 0;
        for (n, &i) in shown.iter().enumerate() {
            let item = &self.items[i];
            if item.section.as_deref() != section {
                section = item.section.as_deref();
                if let Some(s) = section {
                    // A section's items share their icon, so the heading carries it.
                    let icon = item
                        .icon
                        .as_ref()
                        .map(|i| painter.draw(i) + " ")
                        .unwrap_or_default();
                    rows.push((None, format!("{icon}{DIM}{}{RESET}", fit(s, width))));
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
            let icon = item
                .icon
                .as_ref()
                .filter(|_| item.section.is_none())
                .map(|icon| painter.draw(icon) + " ");
            let used = 2 + if icon.is_some() { 3 } else { 0 };
            let room = width.saturating_sub(used + 1);
            let label = fit(&item.label, room);
            let detail_room =
                room.saturating_sub(unicode_width::UnicodeWidthStr::width(label.as_str()) + 2);
            let detail = if item.detail.is_empty() || detail_room < 4 {
                String::new()
            } else if self.details_right {
                let detail = fit(&item.detail, detail_room);
                let gap = detail_room + 1
                    - unicode_width::UnicodeWidthStr::width(detail.as_str()).min(detail_room);
                format!("{}{DIM}{detail}{RESET}", " ".repeat(gap))
            } else {
                format!("  {DIM}{}{RESET}", fit(&item.detail, detail_room))
            };
            let label = if on {
                format!("{INVERSE}{BOLD} {label} {RESET}")
            } else {
                format!(" {label} ")
            };
            out.push(format!(
                "{mark} {}{label}{detail}",
                icon.unwrap_or_default()
            ));
        }
        out
    }
}

/// Whether `needle`'s characters appear in `hay` in order, close together: within a stretch
/// at most twice as long as `needle`, so `drb` finds "dribble" but `pill` doesn't find "pi…l…l"
/// across a whole title.
fn in_order(needle: &str, hay: &str) -> bool {
    let needle: Vec<char> = needle.chars().collect();
    let hay: Vec<char> = hay.chars().collect();
    let Some(&first) = needle.first() else {
        return true;
    };
    let most = needle.len() * 2;
    (0..hay.len()).filter(|&i| hay[i] == first).any(|start| {
        let mut at = start;
        needle[1..]
            .iter()
            .all(|c| match hay[at + 1..].iter().position(|h| h == c) {
                Some(n) => {
                    at += 1 + n;
                    true
                }
                None => false,
            })
            && at - start < most
    })
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

/// Each button's columns on a line, 1-based and inclusive, and its index.
pub type Spans = Vec<((usize, usize), usize)>;

/// Labels drawn as buttons, styled as chips are, two spaces apart, wrapped at `width` columns.
/// The first is bold when it is the one to press. Returns each line, and each button's columns
/// on it, 1-based and inclusive, with its index.
pub fn buttons(
    labels: &[String],
    focused: Option<usize>,
    primary: bool,
    left: usize,
    width: usize,
) -> Vec<(String, Spans)> {
    let mut lines: Vec<(String, Spans)> = Vec::new();
    let mut line = String::new();
    let mut spans = Vec::new();
    let mut used = 0;
    for (i, label) in labels.iter().enumerate() {
        let label = fit(label, width.max(4));
        let w = unicode_width::UnicodeWidthStr::width(label.as_str());
        if used > 0 && used + 2 + w > width {
            lines.push((std::mem::take(&mut line), std::mem::take(&mut spans)));
            used = 0;
        }
        if used > 0 {
            line.push_str("  ");
            used += 2;
        }
        let style = match (focused == Some(i), primary && i == 0) {
            (true, _) => INVERSE,
            (false, true) => BOLD,
            (false, false) => DIM,
        };
        line.push_str(&format!("{style}{label}{RESET}"));
        let start = left + 1 + used;
        spans.push(((start, start + w - 1), i));
        used += w;
    }
    if !spans.is_empty() {
        lines.push((line, spans));
    }
    lines
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
            mode: s["runtimeMode"]
                .as_str()
                .unwrap_or("full-access")
                .to_string(),
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
        let id = self.effort_now(config)?;
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

    /// `Model · Effort · Access` in at most `width` columns, for the one chip under Reply. The
    /// model's name gives way first.
    pub fn summary(&self, config: &Value, width: usize) -> String {
        let tail: String = self
            .effort_label(config)
            .into_iter()
            .chain([self.mode_label()])
            .map(|part| format!(" · {part}"))
            .collect();
        let tail_width = unicode_width::UnicodeWidthStr::width(tail.as_str());
        let model = self.model_label(config);
        let room = width.saturating_sub(tail_width);
        if room == 0 {
            return fit(&format!("{model}{tail}"), width);
        }
        format!("{}{tail}", fit(&model, room).replace(" …", "…"))
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

    /// The current effort's id, for marking it in the menu.
    fn effort_now(&self, config: &Value) -> Option<String> {
        self.effort
            .clone()
            .or_else(|| default_option(self.effort_descriptor(config)?))
    }

    /// A field's chip: the model carries its provider's logo.
    pub fn chip(&self, field: Field, config: &Value, logos: &HashMap<String, Icon>) -> Chip {
        match field {
            Field::Model => Chip {
                icon: logos.get(&self.instance).cloned(),
                label: self.model_label(config),
            },
            Field::Effort => Chip {
                icon: None,
                label: self.effort_label(config).unwrap_or_default(),
            },
            Field::Access => Chip {
                icon: None,
                label: self.mode_label(),
            },
        }
    }

    /// A field's menu items, and the id of the one marked as current.
    pub fn menu(
        &self,
        field: Field,
        config: &Value,
        logos: &HashMap<String, Icon>,
    ) -> (Vec<Item>, Option<String>) {
        match field {
            Field::Model => (
                model_items(config, logos),
                Some(format!("{}/{}", self.instance, self.model)),
            ),
            Field::Effort => (effort_items(config, self), self.effort_now(config)),
            Field::Access => (mode_items(config, self), Some(self.mode.clone())),
        }
    }

    /// Applies the id picked from `field`'s menu.
    pub fn choose(&mut self, field: Field, config: &Value, id: String) {
        match field {
            Field::Model => self.set_model(config, &id),
            Field::Effort => self.effort = Some(id),
            Field::Access => self.mode = id,
        }
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
fn model_items(config: &Value, logos: &HashMap<String, Icon>) -> Vec<Item> {
    let mut items = Vec::new();
    for provider in models::providers(config)
        .iter()
        .filter(|p| models::enabled(p))
    {
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
                model["name"]
                    .as_str()
                    .or(model["slug"].as_str())
                    .unwrap_or("Model"),
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

fn mode_items(config: &Value, settings: &Settings) -> Vec<Item> {
    settings
        .modes(config)
        .into_iter()
        .map(|(id, label)| Item::new(id, label))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_in_order_stay_close() {
        assert!(in_order("drb", "nba 2k27 dribble animation"));
        assert!(in_order("stl", "settle"));
        assert!(!in_order(
            "pill",
            "macos ui inspiration library explorations"
        ));
        assert!(!in_order("zz", "abc"));
    }
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
            vec![
                Item::new("a", "Opus 5.5"),
                Item::new("b", "Haiku 5.5"),
                Item::new("c", "GPT-6"),
            ],
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
    fn the_chip_shortens_the_model_first() {
        let config = json!({"providers": [{"instanceId": "c", "enabled": true, "models": [
            {"slug": "x", "name": "Claude Opus 5.5", "capabilities": {"optionDescriptors": [{"id": "effort", "type": "select",
                "options": [{"id": "medium", "label": "Medium", "isDefault": true}]}]}}
        ]}]});
        let s = Settings {
            instance: "c".into(),
            model: "x".into(),
            effort: None,
            mode: "full-access".into(),
        };
        let wide = "Claude Opus 5.5 · Medium · Full access";
        assert_eq!(s.summary(&config, 80), wide);
        assert_eq!(s.summary(&config, 39), wide);
        assert_eq!(
            s.summary(&config, 36),
            "Claude Opus… · Medium · Full access"
        );
        // Too narrow for the tail: one line, cut, never wider than asked.
        for width in [1, 5, 20, 25] {
            let label = s.summary(&config, width);
            assert!(
                unicode_width::UnicodeWidthStr::width(label.as_str()) <= width,
                "{label}"
            );
        }
    }

    #[test]
    fn buttons_wrap_and_know_their_columns() {
        let labels: Vec<String> = ["Approve", "Always", "Decline"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let lines = buttons(&labels, Some(1), true, 2, 80);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].1, vec![((3, 9), 0), ((12, 17), 1), ((20, 26), 2)]);
        assert!(lines[0].0.contains(&format!("{INVERSE}Always{RESET}")));
        assert!(lines[0].0.starts_with(&format!("{BOLD}Approve")));
        // Too narrow for all three: the third starts a line of its own, at the left again.
        let lines = buttons(&labels, None, false, 0, 16);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].1, vec![((1, 7), 2)]);
    }

    #[test]
    fn a_new_model_keeps_an_effort_it_offers() {
        let config = json!({"providers": [{"instanceId": "c", "enabled": true, "models": [
            {"slug": "x", "name": "X", "capabilities": {"optionDescriptors": [{"id": "effort", "type": "select",
                "options": [{"id": "low"}, {"id": "high", "isDefault": true}]}]}},
            {"slug": "y", "name": "Y", "capabilities": {"optionDescriptors": [{"id": "effort", "type": "select",
                "options": [{"id": "medium", "isDefault": true}]}]}}
        ]}]});
        let mut s = Settings {
            instance: "c".into(),
            model: "x".into(),
            effort: Some("low".into()),
            mode: "full-access".into(),
        };
        s.set_model(&config, "c/y");
        assert_eq!(s.effort, None);
        assert_eq!(s.effort_label(&config).as_deref(), Some("medium"));
        assert_eq!(s.choice().model.as_deref(), Some("c/y"));
    }

    /// Claude has two models with their own efforts; Codex has one with none, and only two of
    /// the runtime modes.
    fn two_providers() -> Value {
        let effort = |options: Value| json!({"optionDescriptors": [{"id": "effort", "type": "select", "options": options}]});
        json!({"providers": [
            {"instanceId": "claude", "displayName": "Claude", "enabled": true, "models": [
                {"slug": "opus", "name": "Opus", "capabilities": effort(json!([
                    {"id": "low", "label": "Low"},
                    {"id": "high", "label": "High", "isDefault": true}]))},
                {"slug": "haiku", "name": "Haiku", "capabilities": effort(json!([
                    {"id": "low", "label": "Low"}, {"id": "medium", "label": "Medium"}]))}]},
            {"instanceId": "off", "displayName": "Off", "enabled": false, "models": [
                {"slug": "hidden", "name": "Hidden"}]},
            {"instanceId": "codex", "displayName": "Codex", "enabled": true,
             "supportedRuntimeModes": ["approval-required", "full-access"], "models": [
                {"slug": "gpt", "name": "GPT"}]}]})
    }

    fn settings(instance: &str, model: &str, effort: Option<&str>, mode: &str) -> Settings {
        Settings {
            instance: instance.into(),
            model: model.into(),
            effort: effort.map(str::to_string),
            mode: mode.into(),
        }
    }

    fn ids(items: &[Item]) -> Vec<&str> {
        items.iter().map(|i| i.id.as_str()).collect()
    }

    #[test]
    fn each_field_lists_its_items_and_marks_the_current_one() {
        let config = two_providers();
        let logos: HashMap<String, Icon> =
            [("claude".to_string(), Icon::Text("C".into(), 2))].into();
        let s = settings("claude", "opus", None, "full-access");

        // Every enabled provider's models, under its name, with its logo; in T3's order.
        let (items, current) = s.menu(Field::Model, &config, &logos);
        assert_eq!(ids(&items), ["claude/opus", "claude/haiku", "codex/gpt"]);
        assert_eq!(current.as_deref(), Some("claude/opus"));
        assert_eq!(items[1].label, "Haiku");
        assert_eq!(items[1].section.as_deref(), Some("Claude"));
        assert_eq!(items[2].section.as_deref(), Some("Codex"));
        assert!(items[0].icon.is_some() && items[2].icon.is_none());

        // No effort chosen yet: the model's default is the one marked.
        let (items, current) = s.menu(Field::Effort, &config, &logos);
        assert_eq!(ids(&items), ["low", "high"]);
        assert_eq!(items[1].label, "High");
        assert_eq!(current.as_deref(), Some("high"));
        let chosen = settings("claude", "opus", Some("low"), "full-access");
        assert_eq!(
            chosen.menu(Field::Effort, &config, &logos).1.as_deref(),
            Some("low")
        );

        // A model with no effort has an empty list, which hides the field.
        let codex = settings("codex", "gpt", None, "full-access");
        assert!(codex.menu(Field::Effort, &config, &logos).0.is_empty());
        assert!(effort_items(&config, &codex).is_empty());
        assert_eq!(codex.chip(Field::Effort, &config, &logos).label, "");

        // Access: the modes the provider supports, in T3's order, the current one marked.
        let (items, current) = s.menu(Field::Access, &config, &logos);
        assert_eq!(
            ids(&items),
            [
                "approval-required",
                "auto-accept-edits",
                "auto",
                "full-access"
            ]
        );
        assert_eq!(current.as_deref(), Some("full-access"));
        let (items, _) = codex.menu(Field::Access, &config, &logos);
        assert_eq!(ids(&items), ["approval-required", "full-access"]);
        assert_eq!(items[0].label, "Supervised");
    }

    #[test]
    fn chips_say_what_the_menus_mark() {
        let config = two_providers();
        let logos: HashMap<String, Icon> =
            [("claude".to_string(), Icon::Text("C".into(), 2))].into();
        let s = settings("claude", "opus", None, "approval-required");
        let model = s.chip(Field::Model, &config, &logos);
        assert_eq!((model.label.as_str(), model.icon.is_some()), ("Opus", true));
        let effort = s.chip(Field::Effort, &config, &logos);
        assert_eq!(
            (effort.label.as_str(), effort.icon.is_none()),
            ("High", true)
        );
        assert_eq!(s.chip(Field::Access, &config, &logos).label, "Supervised");
        // A model T3 no longer lists shows as its slug, and has no effort to show.
        let gone = settings("claude", "retired", None, "full-access");
        assert_eq!(gone.chip(Field::Model, &config, &logos).label, "retired");
        assert_eq!(gone.effort_label(&config), None);
    }

    #[test]
    fn choosing_changes_only_what_the_field_holds() {
        let config = two_providers();
        let mut s = settings("claude", "opus", Some("low"), "auto");
        s.choose(Field::Effort, &config, "high".into());
        assert_eq!(s, settings("claude", "opus", Some("high"), "auto"));
        s.choose(Field::Access, &config, "approval-required".into());
        assert_eq!(s.mode, "approval-required");

        // A model that offers the effort keeps it; one that doesn't drops it.
        s.choose(Field::Model, &config, "claude/haiku".into());
        assert_eq!((s.model.as_str(), s.effort.as_deref()), ("haiku", None));
        s.effort = Some("medium".into());
        s.choose(Field::Model, &config, "claude/opus".into());
        assert_eq!(s.effort, None);
        // Access the new provider can't run is corrected to one it can.
        s.mode = "auto".into();
        s.choose(Field::Model, &config, "codex/gpt".into());
        assert_eq!(
            (s.instance.as_str(), s.mode.as_str()),
            ("codex", "approval-required")
        );
        // An id that isn't `instance/slug` changes nothing.
        let before = s.clone();
        s.choose(Field::Model, &config, "nonsense".into());
        assert_eq!(s, before);
    }

    /// Logos arrive after the menu opened. Swapping the items in leaves the filter, and the
    /// row the cursor is on, where they were.
    #[test]
    fn logos_arriving_leave_the_menu_where_it_was() {
        let config = two_providers();
        let s = settings("claude", "opus", None, "full-access");
        let (items, current) = s.menu(Field::Model, &config, &HashMap::new());
        let mut menu = Menu::new(items, current.as_deref());
        for c in "hai".chars() {
            menu.key(key(KeyCode::Char(c)));
        }
        let shown = menu.shown();
        let selected = menu.selected;
        let logos: HashMap<String, Icon> =
            [("claude".to_string(), Icon::Text("C".into(), 2))].into();
        menu.items = s.menu(Field::Model, &config, &logos).0;
        assert_eq!(menu.filter, "hai");
        assert_eq!((menu.shown(), menu.selected), (shown, selected));
        assert!(menu.items.iter().any(|i| i.icon.is_some()));
        assert!(
            matches!(menu.key(key(KeyCode::Enter)), MenuKey::Chose(id) if id == "claude/haiku")
        );
    }
}
