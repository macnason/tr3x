//! Project icons and provider logos, as T3 shows them. A project's icon is the one chosen in
//! its settings (emoji or monogram), else the favicon T3 finds in the repository, which T3
//! serves through `assets.createUrl`, else the monogram T3 derives from the title. Pictures are
//! cached as PNG and drawn with kitty graphics, two cells wide and one tall; anything else is
//! two coloured characters.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde_json::{Value, json};
use t3term::client::Client;

use crate::chat::{BOLD, RESET};

/// What draws in an icon's two cells.
#[derive(Clone, Debug, PartialEq)]
pub enum Icon {
    Picture(PathBuf),
    /// Up to two characters in one of the terminal's 16 colours.
    Text(String, u8),
}

/// T3's project icon colours, in the order its colour hash indexes them
/// (apps/web/src/projectIconColors.ts), each with the nearest of the terminal's 16 colours.
const COLORS: [(&str, u8); 18] = [
    ("gray", 8),
    ("red", 1),
    ("orange", 3),
    ("amber", 3),
    ("yellow", 3),
    ("lime", 2),
    ("green", 2),
    ("emerald", 2),
    ("teal", 6),
    ("cyan", 6),
    ("sky", 6),
    ("blue", 4),
    ("indigo", 4),
    ("violet", 5),
    ("purple", 5),
    ("fuchsia", 5),
    ("pink", 5),
    ("rose", 1),
];

fn ansi(color: &str) -> u8 {
    COLORS
        .iter()
        .find(|(name, _)| *name == color)
        .map_or(4, |(_, c)| *c)
}

/// `projectMonogram` in apps/web/src/projectIdentity.ts: the first word's first glyph, then a
/// digit from that word, else the last word's first glyph, else the word's last glyph.
pub fn monogram(title: &str) -> String {
    let words: Vec<Vec<char>> = title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.chars().collect())
        .collect();
    let Some(first_word) = words.first() else {
        return "PR".into();
    };
    let first = first_word[0];
    let second = first_word[1..]
        .iter()
        .find(|c| c.is_numeric())
        .copied()
        .or_else(|| {
            if words.len() > 1 {
                words.last().and_then(|w| w.first().copied())
            } else {
                first_word.last().copied()
            }
        })
        .unwrap_or(first);
    format!("{first}{second}")
        .to_uppercase()
        .chars()
        .take(2)
        .collect()
}

/// `projectColor`: a hash of the lowercase title picks one of the 18 colours.
pub fn monogram_color(title: &str) -> &'static str {
    let seed = title.trim().to_lowercase();
    let seed = if seed.is_empty() { "project".into() } else { seed };
    let mut index: u32 = 0;
    for c in seed.chars() {
        index = (index * 31 + c as u32) % COLORS.len() as u32;
    }
    COLORS[index as usize].0
}

/// The icon from the project's own setting, or its monogram. None means look for a favicon.
pub fn chosen(project: &Value) -> Option<Icon> {
    let icon = &project["projectIcon"];
    let title = project["title"].as_str().unwrap_or("Project");
    match icon["kind"].as_str()? {
        "emoji" => Some(Icon::Text(
            icon["emoji"].as_str().unwrap_or("•").to_string(),
            7,
        )),
        "monogram" => Some(Icon::Text(
            icon["text"].as_str().map_or_else(|| monogram(title), |t| t.chars().take(2).collect()),
            ansi(icon["color"].as_str().unwrap_or("blue")),
        )),
        // A Lucide glyph has no terminal form; its colour on the monogram keeps the likeness.
        "lucide" => Some(Icon::Text(
            monogram(title),
            ansi(icon["color"].as_str().unwrap_or("gray")),
        )),
        _ => None,
    }
}

pub fn fallback(project: &Value) -> Icon {
    let title = project["title"].as_str().unwrap_or("Project");
    Icon::Text(monogram(title), ansi(monogram_color(title)))
}

fn cache() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join("Library/Caches/t3rex/icons")
}

/// `name` made safe as a file name.
fn file_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else { '_' })
        .collect()
}

/// A PNG of `src`: the file itself when it is one, else a copy `sips` converts, which reads
/// SVG, ICO, GIF, WebP and the rest with transparency kept.
async fn as_png(src: &Path, dest: &Path) -> Option<PathBuf> {
    let head = tokio::fs::read(src).await.ok()?;
    if head.starts_with(b"\x89PNG") {
        tokio::fs::rename(src, dest).await.ok()?;
        return Some(dest.to_path_buf());
    }
    let ok = tokio::process::Command::new("/usr/bin/sips")
        .args(["-s", "format", "png", "-Z", "64"])
        .arg(src)
        .arg("--out")
        .arg(dest)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success());
    let _ = tokio::fs::remove_file(src).await;
    (ok && dest.is_file()).then(|| dest.to_path_buf())
}

/// The project's icon as T3 would draw it. Fetches and caches its favicon when it has one.
pub async fn project(client: &Client, project: &Value) -> Icon {
    if let Some(icon) = chosen(project) {
        return icon;
    }
    let (Some(id), Some(root)) = (project["id"].as_str(), project["workspaceRoot"].as_str()) else {
        return fallback(project);
    };
    let mut resource = json!({"_tag": "project-favicon", "cwd": root});
    if let Some(path) = project["faviconPath"].as_str() {
        resource["path"] = json!(path);
    }
    let Ok(rpc) = client.rpc().await else {
        return fallback(project);
    };
    let Ok(asset) = rpc
        .call(
            "assets.createUrl",
            json!({"resource": resource}),
            std::time::Duration::from_secs(10),
        )
        .await
    else {
        return fallback(project);
    };
    let Some(url) = asset["relativeUrl"].as_str() else {
        return fallback(project);
    };
    if url.ends_with("/project-favicon-missing") {
        return fallback(project);
    }
    // The last segment is `v<sha256>-<name>`, so it names this version of the file.
    let version = url.rsplit('/').next().unwrap_or("icon");
    let dir = cache();
    let png = dir.join(format!("{}-{}.png", file_name(id), file_name(version)));
    if png.is_file() {
        return Icon::Picture(png);
    }
    let _ = tokio::fs::create_dir_all(&dir).await;
    let raw = dir.join(format!("{}.download", file_name(id)));
    let fetched = tokio::process::Command::new("/usr/bin/curl")
        .args(["-fsS", "--max-time", "10", "-o"])
        .arg(&raw)
        .arg(format!("{}{url}", client.runtime.origin.trim_end_matches('/')))
        .status()
        .await
        .is_ok_and(|s| s.success());
    if !fetched {
        return fallback(project);
    }
    match as_png(&raw, &png).await {
        Some(path) => Icon::Picture(path),
        None => fallback(project),
    }
}

/// Every project's icon, by project id.
pub async fn projects(client: &Client, projects: &[Value]) -> HashMap<String, Icon> {
    let mut out = HashMap::new();
    for p in projects {
        if let Some(id) = p["id"].as_str() {
            out.insert(id.to_string(), project(client, p).await);
        }
    }
    out
}

const LOGOS: &[(&str, &str)] = &[
    ("claudeAgent", include_str!("../assets/logos/claudeAgent.svg")),
    ("codex", include_str!("../assets/logos/codex.svg")),
    ("cursor", include_str!("../assets/logos/cursor.svg")),
    ("grok", include_str!("../assets/logos/grok.svg")),
    ("pi", include_str!("../assets/logos/pi.svg")),
    ("fx", include_str!("../assets/logos/fx.svg")),
    ("opencode", include_str!("../assets/logos/opencode.svg")),
    ("muse", include_str!("../assets/logos/muse.svg")),
    ("acpRegistry", include_str!("../assets/logos/acpRegistry.svg")),
];

/// The provider's logo as T3 shows it, for the Mac's appearance, or its initials.
pub async fn provider(provider: &Value, dark: bool) -> Icon {
    let driver = provider["driver"]
        .as_str()
        .or(provider["instanceId"].as_str())
        .unwrap_or_default();
    let Some((_, svg)) = LOGOS.iter().find(|(d, _)| *d == driver) else {
        let name = provider["displayName"].as_str().unwrap_or(driver);
        return Icon::Text(monogram(name), 7);
    };
    let theme = if dark { "dark" } else { "light" };
    let dir = cache();
    let png = dir.join(format!("logo-{driver}-{theme}.png"));
    if png.is_file() {
        return Icon::Picture(png);
    }
    let (mono, mono2) = if dark {
        ("#F5F5F5", "#4B4646")
    } else {
        ("#0F0F0F", "#CFCECD")
    };
    let _ = tokio::fs::create_dir_all(&dir).await;
    let src = dir.join(format!("logo-{driver}-{theme}.svg"));
    let svg = svg.replace("#MONO2#", mono2).replace("#MONO#", mono);
    if tokio::fs::write(&src, svg).await.is_err() {
        return Icon::Text(monogram(driver), 7);
    }
    match as_png(&src, &png).await {
        Some(path) => Icon::Picture(path),
        None => Icon::Text(monogram(driver), 7),
    }
}

/// Draws icons as kitty graphics placements. Each picture goes to the terminal once, under an
/// id, and every frame places it again after `clear` takes the last frame's placements away.
#[derive(Default)]
pub struct Painter {
    ids: HashMap<PathBuf, u32>,
}

impl Painter {
    /// Removes every placement this pane made, keeping the pictures.
    pub fn clear() -> &'static str {
        "\x1b_Ga=d,d=a,q=2\x1b\\"
    }

    /// The icon at the cursor, two cells wide, leaving the cursor after it.
    pub fn draw(&mut self, icon: &Icon) -> String {
        match icon {
            Icon::Text(text, color) => {
                let w = unicode_width::UnicodeWidthStr::width(text.as_str());
                let pad = " ".repeat(2usize.saturating_sub(w));
                let fg = if *color >= 8 { 90 + (color - 8) as u32 } else { 30 + *color as u32 };
                format!("\x1b[{fg}m{BOLD}{text}{RESET}{pad}")
            }
            Icon::Picture(path) => {
                let mut out = String::new();
                let next = self.ids.len() as u32 + 7001;
                let id = *self.ids.entry(path.clone()).or_insert_with(|| {
                    let name = base64::engine::general_purpose::STANDARD
                        .encode(path.to_string_lossy().as_bytes());
                    out.push_str(&format!("\x1b_Ga=t,f=100,t=f,i={next},q=2;{name}\x1b\\"));
                    next
                });
                out.push_str(&format!("\x1b_Ga=p,i={id},c=2,r=1,C=1,q=2\x1b\\\x1b[2C"));
                out
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monograms_match_t3() {
        assert_eq!(monogram("t3rex"), "T3");
        assert_eq!(monogram("MacStack"), "MK");
        assert_eq!(monogram("Up Bank web"), "UW");
        assert_eq!(monogram("  "), "PR");
    }

    #[test]
    fn chosen_icons() {
        let p = json!({"title": "No project", "projectIcon": {"kind": "lucide", "name": "x", "color": "gray"}});
        assert_eq!(chosen(&p), Some(Icon::Text("NP".into(), 8)));
        let p = json!({"title": "A", "projectIcon": {"kind": "emoji", "emoji": "🦖"}});
        assert_eq!(chosen(&p), Some(Icon::Text("🦖".into(), 7)));
        assert_eq!(chosen(&json!({"title": "A", "projectIcon": null})), None);
    }
}
