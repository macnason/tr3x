//! Pictures in the conversation pane, drawn with the kitty graphics protocol, which Rex's
//! terminal supports. Local image files show inline. An HTML embed from T3's `html_render`
//! shows as a snapshot taken with a headless Chromium when one is installed, under a titled
//! link that opens the thread in T3 Code, where the page is interactive.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use base64::Engine;
use t3term::projection::ThreadState;

/// An embed T3 rendered as HTML: its attachment and title.
#[derive(Clone, Debug)]
pub struct Embed {
    pub attachment: String,
    pub title: String,
    pub height: u64,
}

/// The `html_render` calls in a thread, by item id.
pub fn embeds(state: &ThreadState) -> HashMap<String, Embed> {
    let mut out = HashMap::new();
    for item in state.list("turnItems") {
        let render = &item["output"]["htmlRender"];
        let (Some(id), Some(attachment)) = (item["id"].as_str(), render["attachmentId"].as_str())
        else {
            continue;
        };
        out.insert(
            id.to_string(),
            Embed {
                attachment: attachment.to_string(),
                title: render["title"].as_str().unwrap_or("HTML").to_string(),
                height: render["height"].as_u64().unwrap_or(800).clamp(120, 4000),
            },
        );
    }
    out
}

pub struct Media {
    /// T3's attachments folder, where each embed's HTML is `<attachment>.html`.
    pub attachments: Option<PathBuf>,
    cache: PathBuf,
    /// Snapshots match the Mac's appearance, as the closest guess at the terminal's.
    pub dark: bool,
    /// A cell's width over its height, for working out how many rows a picture takes.
    pub cell_ratio: f64,
    pub snapshots: HashMap<String, PathBuf>,
    asked: HashSet<String>,
    chrome: Option<PathBuf>,
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// A headless Chromium: Playwright's, or a Chrome in /Applications.
fn find_chrome() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("T3REX_CHROME") {
        return Some(PathBuf::from(path));
    }
    let playwright = home().join("Library/Caches/ms-playwright");
    if let Ok(dirs) = std::fs::read_dir(&playwright) {
        let mut found: Vec<PathBuf> = dirs
            .flatten()
            .map(|d| d.path())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("chromium_headless_shell"))
            })
            .filter_map(|p| {
                std::fs::read_dir(&p)
                    .ok()?
                    .flatten()
                    .map(|d| d.path().join("chrome-headless-shell"))
                    .find(|b| b.is_file())
            })
            .collect();
        found.sort();
        if let Some(path) = found.pop() {
            return Some(path);
        }
    }
    [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}

fn mac_is_dark() -> bool {
    std::process::Command::new("defaults")
        .args(["read", "-g", "AppleInterfaceStyle"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "Dark")
}

impl Media {
    pub fn new(state_dir: Option<&Path>) -> Media {
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let ratio = crossterm::terminal::window_size()
            .ok()
            .filter(|w| w.width > 0 && w.height > 0 && cols > 0 && rows > 0)
            .map(|w| (w.width as f64 / cols as f64) / (w.height as f64 / rows as f64))
            .unwrap_or(0.5);
        Media {
            attachments: state_dir
                .map(|d| d.join("attachments"))
                .filter(|d| d.is_dir()),
            cache: home().join("Library/Caches/t3rex"),
            dark: mac_is_dark(),
            cell_ratio: ratio,
            snapshots: HashMap::new(),
            asked: HashSet::new(),
            chrome: find_chrome(),
        }
    }

    pub fn can_snapshot(&self) -> bool {
        self.chrome.is_some() && self.attachments.is_some()
    }

    /// Starts a snapshot of `embed` unless one is cached or under way. `done` hears the
    /// attachment and its PNG once it exists.
    pub fn ask(
        &mut self,
        embed: &Embed,
        done: tokio::sync::mpsc::UnboundedSender<(String, PathBuf)>,
    ) {
        // An attachment id names a file in T3's folder and nothing outside it.
        let safe = !embed.attachment.is_empty()
            && embed
                .attachment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
            && !embed.attachment.contains("..");
        if !safe {
            return;
        }
        if self.snapshots.contains_key(&embed.attachment)
            || !self.asked.insert(embed.attachment.clone())
        {
            return;
        }
        let (Some(chrome), Some(dir)) = (self.chrome.clone(), self.attachments.clone()) else {
            return;
        };
        let theme = if self.dark { "dark" } else { "light" };
        let png = self.cache.join(format!("{}-{theme}.png", embed.attachment));
        if png.is_file() {
            self.snapshots.insert(embed.attachment.clone(), png);
            return;
        }
        let (embed, dark, cache) = (embed.clone(), self.dark, self.cache.clone());
        tokio::spawn(async move {
            let Ok(html) =
                tokio::fs::read_to_string(dir.join(format!("{}.html", embed.attachment))).await
            else {
                return;
            };
            let _ = tokio::fs::create_dir_all(&cache).await;
            let page = cache.join(format!("{}-{theme}.html", embed.attachment));
            if tokio::fs::write(&page, wrap(&html, dark)).await.is_err() {
                return;
            }
            let ok = tokio::process::Command::new(chrome)
                .args([
                    "--headless",
                    "--disable-gpu",
                    "--hide-scrollbars",
                    "--force-device-scale-factor=2",
                ])
                // Embeds that follow prefers-color-scheme follow the Mac too.
                .args(if dark {
                    [
                        "--force-dark-mode",
                        "--blink-settings=preferredColorScheme=0",
                    ]
                } else {
                    [
                        "--disable-features=WebContentsForceDark",
                        "--blink-settings=preferredColorScheme=1",
                    ]
                })
                .arg(format!("--window-size=728,{}", embed.height))
                // Its own profile, so a running Chrome never takes the page as a new tab.
                .arg(format!(
                    "--user-data-dir={}",
                    cache.join("chromium-profile").display()
                ))
                .arg("--no-first-run")
                .arg(format!("--screenshot={}", png.display()))
                .arg(format!(
                    "file://{}",
                    crate::chat::percent(&page.to_string_lossy())
                ))
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .status()
                .await
                .is_ok_and(|s| s.success());
            let _ = tokio::fs::remove_file(&page).await;
            if ok && png.is_file() {
                let _ = done.send((embed.attachment, png));
            }
        });
    }

    /// A picture `cols` columns wide, as the kitty graphics sequence and the rows it takes.
    /// The terminal reads the file itself, so nothing large goes through the pty.
    pub fn picture(&self, path: &Path, cols: usize) -> Option<(String, usize)> {
        let (w, h) = png_size(path).or_else(|| jpeg_size(path))?;
        if w == 0 || h == 0 {
            return None;
        }
        // Never wider than the picture is at 2x, so small ones stay small.
        let cell_px = 8.0 * 2.0;
        let cols = cols.min(((w as f64 / cell_px).ceil() as usize).max(8));
        let rows =
            ((cols as f64 * self.cell_ratio * h as f64 / w as f64).round() as usize).clamp(1, 60);
        let name =
            base64::engine::general_purpose::STANDARD.encode(path.to_string_lossy().as_bytes());
        // f=100 PNG (the terminal sniffs JPEG too), t=f a file, C=1 leave the cursor alone, q=2 quiet.
        let seq = format!("\x1b_Ga=T,f=100,t=f,c={cols},r={rows},C=1,q=2;{name}\x1b\\");
        Some((seq, rows))
    }
}

/// The page with T3's theme variables, which embeds use for their colours.
fn wrap(html: &str, dark: bool) -> String {
    let vars = if dark {
        "--background:#0d0d0f;--foreground:#ececec;--muted:#1c1c20;--muted-foreground:#9a9aa2;--card:#151518;--card-foreground:#ececec;--popover:#151518;--secondary:#1c1c20;--secondary-foreground:#ececec;--border:#2a2a30;--ring:#5b8cff;--primary:#ececec;--primary-foreground:#0d0d0f;--accent:#1f2a44;--accent-foreground:#cfe0ff;--destructive:#ff5c5c;--warning:#f5a524;--success:#3fb950;--info:#5b8cff;--code-background:#151518;--chart-1:#5b8cff;--chart-2:#3fb950;--chart-3:#f5a524;--chart-4:#d06cff;--chart-5:#ff5c8a;--chart-6:#2ec4b6;--radius:10px;color-scheme:dark"
    } else {
        "--background:#ffffff;--foreground:#18181b;--muted:#f4f4f5;--muted-foreground:#71717a;--card:#fafafa;--card-foreground:#18181b;--popover:#ffffff;--secondary:#f4f4f5;--secondary-foreground:#18181b;--border:#e4e4e7;--ring:#2563eb;--primary:#18181b;--primary-foreground:#ffffff;--accent:#eff4ff;--accent-foreground:#1d4ed8;--destructive:#dc2626;--warning:#d97706;--success:#16a34a;--info:#2563eb;--code-background:#f4f4f5;--chart-1:#2563eb;--chart-2:#16a34a;--chart-3:#d97706;--chart-4:#9333ea;--chart-5:#e11d48;--chart-6:#0d9488;--radius:10px;color-scheme:light"
    };
    let base = format!(
        "<style>:root{{{vars};--font-sans:-apple-system,BlinkMacSystemFont,\"SF Pro Text\",system-ui,sans-serif;--font-mono:\"SF Mono\",ui-monospace,Menlo,monospace}}html{{padding:8px}}body{{margin:0;background:var(--background);color:var(--foreground);font-family:var(--font-sans);font-size:14px}}</style>"
    );
    match html.find("<head>") {
        Some(at) => format!("{}{base}{}", &html[..at + 6], &html[at + 6..]),
        None => format!("{base}{html}"),
    }
}

fn png_size(path: &Path) -> Option<(u32, u32)> {
    use std::io::Read;
    let mut head = [0u8; 24];
    std::fs::File::open(path).ok()?.read_exact(&mut head).ok()?;
    if &head[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    Some((
        u32::from_be_bytes(head[16..20].try_into().ok()?),
        u32::from_be_bytes(head[20..24].try_into().ok()?),
    ))
}

fn jpeg_size(path: &Path) -> Option<(u32, u32)> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut at = 2;
    while at + 9 < bytes.len() {
        if bytes[at] != 0xFF {
            return None;
        }
        let marker = bytes[at + 1];
        let len = u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]) as usize;
        if (0xC0..=0xCF).contains(&marker) && ![0xC4, 0xC8, 0xCC].contains(&marker) {
            let h = u16::from_be_bytes([bytes[at + 5], bytes[at + 6]]) as u32;
            let w = u16::from_be_bytes([bytes[at + 7], bytes[at + 8]]) as u32;
            return Some((w, h));
        }
        at += 2 + len;
    }
    None
}

/// A Markdown image line, `![alt](/path)`, that names a file on this Mac.
pub fn local_image(line: &str) -> Option<(String, PathBuf)> {
    let line = line.trim();
    let rest = line.strip_prefix("![")?;
    let (alt, rest) = rest.split_once("](")?;
    let path = rest.strip_suffix(')')?;
    let path = path.strip_prefix("file://").unwrap_or(path);
    let path = PathBuf::from(path);
    (path.is_absolute() && path.is_file()).then(|| (alt.to_string(), path))
}
