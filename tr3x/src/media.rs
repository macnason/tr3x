//! Pictures in the conversation pane, drawn with the kitty graphics protocol, which Rex's
//! terminal supports. Local image files show inline. An HTML embed from T3's `html_render`
//! shows as a snapshot taken with a headless Chromium when one is installed, under a titled
//! link that opens the thread in T3 Code, where the page is interactive.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::os::fd::AsRawFd;
use std::time::Duration;

use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

use base64::Engine;
use t3client::projection::ThreadState;

/// How much picture a pane holds at once. The terminal keeps each picture as RGBA pixels in
/// the snapshot it sends when Rex's app connects, and the app shows a pane whose snapshot is
/// past about 64 MiB as Stale. Past this the oldest pictures are freed.
const PICTURE_BUDGET: u64 = 16 << 20;
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(30);
const RETRY_AFTER: Duration = Duration::from_secs(10);

pub enum Ready {
    Snapshot {
        attachment: String,
        path: Option<PathBuf>,
        generation: u64,
    },
    Picture {
        path: PathBuf,
    },
}

/// Frees every picture in the terminal, including ones scrolled into the scrollback, which
/// `a=d,d=A` leaves in memory.
const FREE_ALL: &str = "\x1b_Ga=d,d=R,x=1,y=4294967295,q=2\x1b\\";

/// The pictures in the terminal, oldest first, with their ids and sizes in bytes.
#[derive(Default)]
struct Held {
    next_id: u32,
    pictures: VecDeque<(u32, u64)>,
    bytes: u64,
}

/// An embed T3 rendered as HTML: its attachment and title.
#[derive(Clone, Debug)]
pub struct Embed {
    pub attachment: String,
    pub title: String,
    pub height: u64,
}

impl Media {
    /// The page an `html_render` embed shows, when it's on this Mac.
    pub fn html_file(&self, attachment: &str) -> Option<PathBuf> {
        let file = self
            .attachments
            .as_ref()?
            .join(format!("{attachment}.html"));
        file.is_file().then_some(file)
    }
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
    /// Snapshots match the terminal's scheme, else the Mac's appearance.
    pub dark: bool,
    /// A cell's width over its height, for working out how many rows a picture takes.
    pub cell_ratio: f64,
    pub snapshots: HashMap<String, PathBuf>,
    asked: HashMap<String, Option<tokio::task::JoinHandle<()>>>,
    pending_pictures: RefCell<HashMap<PathBuf, bool>>,
    done: Option<mpsc::UnboundedSender<Ready>>,
    generation: u64,
    chrome: Option<PathBuf>,
    held: RefCell<Held>,
}

/// A headless Chromium: Playwright's, or a Chrome or Chromium the machine has.
pub(crate) fn find_chrome() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("TR3X_CHROME") {
        return Some(PathBuf::from(path));
    }
    let playwright = crate::platform::playwright();
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
    crate::platform::browsers()
        .into_iter()
        .find(|p| p.is_file())
}

pub fn mac_is_dark() -> bool {
    std::process::Command::new("defaults")
        .args(["read", "-g", "AppleInterfaceStyle"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "Dark")
}

impl Media {
    pub fn new(state_dir: Option<&Path>) -> Media {
        Media {
            attachments: state_dir
                .map(|d| d.join("attachments"))
                .filter(|d| d.is_dir()),
            cache: crate::platform::cache(),
            dark: crate::scheme::dark(),
            cell_ratio: current_cell_ratio().unwrap_or(0.5),
            snapshots: HashMap::new(),
            asked: HashMap::new(),
            pending_pictures: RefCell::default(),
            done: None,
            generation: 0,
            chrome: find_chrome(),
            held: RefCell::default(),
        }
    }

    /// Follows a screen that turned light or dark: snapshots are taken again in its scheme.
    pub fn set_dark(&mut self, dark: bool) {
        self.dark = dark;
        self.generation = self.generation.wrapping_add(1);
        self.snapshots.clear();
        self.cancel_captures();
    }

    /// Frees every picture, for printing the thread again from the top.
    pub fn free_all(&self) -> &'static str {
        let mut held = self.held.borrow_mut();
        held.pictures.clear();
        held.bytes = 0;
        FREE_ALL
    }

    pub fn can_snapshot(&self) -> bool {
        self.chrome.is_some() && self.attachments.is_some()
    }

    pub fn listen(&mut self, done: mpsc::UnboundedSender<Ready>) {
        self.done = Some(done);
    }

    fn cancel_captures(&mut self) {
        for capture in self.asked.values_mut() {
            if let Some(job) = capture.take() {
                job.abort();
            }
        }
        self.asked.clear();
    }

    /// Returns whether a completed job changes this pane's current pictures.
    pub fn ready(&mut self, ready: Ready) -> bool {
        match ready {
            Ready::Snapshot {
                attachment,
                path,
                generation,
            } => {
                if generation != self.generation {
                    return false;
                }
                if let Some(capture) = self.asked.get_mut(&attachment) {
                    *capture = None;
                }
                if let Some(path) = path {
                    self.snapshots.insert(attachment, path);
                }
                true
            }
            Ready::Picture { path } => {
                self.pending_pictures.borrow_mut().insert(path, false);
                true
            }
        }
    }

    /// Starts a snapshot unless cached, queued, or tried twice in this scheme.
    pub fn ask(&mut self, embed: &Embed, done: mpsc::UnboundedSender<Ready>) {
        let safe = !embed.attachment.is_empty()
            && embed
                .attachment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
            && !embed.attachment.contains("..");
        if !safe || self.snapshots.contains_key(&embed.attachment) {
            return;
        }
        if self.asked.contains_key(&embed.attachment) {
            return;
        }
        if self.asked.values().filter(|c| c.is_some()).count() >= 2 {
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
        let generation = self.generation;
        let (embed, dark, cache) = (embed.clone(), self.dark, self.cache.clone());
        let attachment = embed.attachment.clone();
        let job = tokio::spawn(async move {
            let mut path = None;
            for attempt in 0..2 {
                if attempt > 0 {
                    tokio::time::sleep(RETRY_AFTER).await;
                }
                let Some(_slot) = lock_slot(&cache).await else {
                    continue;
                };
                path = tokio::time::timeout(
                    CAPTURE_TIMEOUT,
                    capture_locked(&chrome, &dir, &cache, &embed, dark, &png),
                )
                .await
                .ok()
                .flatten();
                if path.is_some() {
                    break;
                }
            }
            let _ = done.send(Ready::Snapshot {
                attachment: embed.attachment,
                path,
                generation,
            });
        });
        self.asked.insert(attachment, Some(job));
    }

    /// A picture `cols` columns wide, as the kitty graphics sequence and the rows it takes.
    /// The terminal reads the file itself, so nothing large goes through the pty. None for a
    /// picture bigger than a pane holds.
    pub fn picture(&self, path: &Path, cols: usize) -> Option<(String, usize)> {
        let (w, h) = png_size(path).or_else(|| jpeg_size(path))?;
        if w == 0 || h == 0 {
            return None;
        }
        // Never wider than the original at 2x; fit tall pictures before rounding rows.
        let ratio = current_cell_ratio().unwrap_or(self.cell_ratio);
        let cols = cols.min(((w as f64 / 16.0).ceil() as usize).max(8));
        let cols = cols.min((60.0 * w as f64 / (ratio * h as f64)).floor().max(1.0) as usize);
        let rows = (cols as f64 * ratio * h as f64 / w as f64)
            .round()
            .clamp(1.0, 60.0) as usize;
        let (path, pixels) = self.display_picture(path, w, h, cols)?;
        let bytes = pixels.0 as u64 * pixels.1 as u64 * 4;
        if bytes > PICTURE_BUDGET {
            return None;
        }
        // Room for it, freeing the oldest pictures. Their rows stay, blank.
        let mut seq = String::new();
        let mut held = self.held.borrow_mut();
        while held.bytes + bytes > PICTURE_BUDGET
            && let Some((id, size)) = held.pictures.pop_front()
        {
            seq.push_str(&format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\"));
            held.bytes -= size;
        }
        held.next_id += 1;
        let id = held.next_id;
        held.pictures.push_back((id, bytes));
        held.bytes += bytes;
        let name =
            base64::engine::general_purpose::STANDARD.encode(path.to_string_lossy().as_bytes());
        // i= its own id, so it can be freed. f=100 PNG (the terminal sniffs JPEG too), t=f a
        // file, C=1 leave the cursor alone, q=2 quiet.
        seq.push_str(&format!(
            "\x1b_Ga=T,i={id},f=100,t=f,c={cols},r={rows},C=1,q=2;{name}\x1b\\"
        ));
        Some((seq, rows))
    }

    fn display_picture(
        &self,
        path: &Path,
        w: u32,
        h: u32,
        cols: usize,
    ) -> Option<(PathBuf, (u32, u32))> {
        let bounds = ((cols.saturating_mul(16) as u32).clamp(16, 1536), 1920);
        if w <= bounds.0 && h <= bounds.1 {
            return Some((path.to_path_buf(), (w, h)));
        }
        let meta = std::fs::metadata(path).ok()?;
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        path.hash(&mut hash);
        meta.len().hash(&mut hash);
        meta.modified().ok()?.hash(&mut hash);
        let cached = self.cache.join(format!(
            "display-{:x}-{}x{}.png",
            hash.finish(),
            bounds.0,
            bounds.1
        ));
        if let Some(size) = png_size(&cached) {
            return Some((cached, size));
        }
        let done = self.done.as_ref()?.clone();
        let mut pending = self.pending_pictures.borrow_mut();
        if pending.contains_key(&cached)
            || pending.values().filter(|running| **running).count() >= 2
        {
            return None;
        }
        pending.insert(cached.clone(), true);
        {
            let source = path.to_path_buf();
            tokio::task::spawn_blocking(move || {
                let _ = display_derivative(&source, &cached, bounds);
                let _ = done.send(Ready::Picture { path: cached });
            });
        }
        None
    }
}

impl Drop for Media {
    fn drop(&mut self) {
        self.cancel_captures();
    }
}

fn display_derivative(source: &Path, destination: &Path, bounds: (u32, u32)) -> Option<()> {
    let image =
        image::open(source)
            .ok()?
            .resize(bounds.0, bounds.1, image::imageops::FilterType::Triangle);
    std::fs::create_dir_all(destination.parent()?).ok()?;
    let temporary = destination.with_extension(format!("{}.png", uuid::Uuid::new_v4()));
    let result = image
        .save(&temporary)
        .ok()
        .and_then(|_| std::fs::rename(&temporary, destination).ok());
    let _ = std::fs::remove_file(temporary);
    result
}

struct CaptureFiles(PathBuf);

impl Drop for CaptureFiles {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One capture slot across all panes and processes. The OS drops the flock if a holder dies.
async fn lock_slot(cache: &Path) -> Option<std::fs::File> {
    std::fs::create_dir_all(cache).ok()?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(cache.join("capture.lock"))
        .ok()?;
    loop {
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Some(lock);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::WouldBlock {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
async fn capture(
    chrome: &Path,
    attachments: &Path,
    cache: &Path,
    embed: &Embed,
    dark: bool,
    png: &Path,
) -> Option<PathBuf> {
    let _slot = lock_slot(cache).await?;
    capture_locked(chrome, attachments, cache, embed, dark, png).await
}

/// Runs with the slot held; the deadline covers Chromium only, not the wait in the queue.
async fn capture_locked(
    chrome: &Path,
    attachments: &Path,
    cache: &Path,
    embed: &Embed,
    dark: bool,
    png: &Path,
) -> Option<PathBuf> {
    if png.is_file() {
        return Some(png.to_path_buf());
    }
    let html = tokio::fs::read_to_string(attachments.join(format!("{}.html", embed.attachment)))
        .await
        .ok()?;
    let files = CaptureFiles(cache.join(format!("capture-{}", uuid::Uuid::new_v4())));
    tokio::fs::create_dir_all(&files.0).await.ok()?;
    let page = files.0.join("page.html");
    let screenshot = files.0.join("snapshot.png");
    tokio::fs::write(&page, wrap(&html, dark)).await.ok()?;
    let ok = tokio::process::Command::new(chrome)
        .args([
            "--headless",
            "--disable-gpu",
            "--hide-scrollbars",
            "--force-device-scale-factor=2",
        ])
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
        .arg(format!(
            "--user-data-dir={}",
            files.0.join("profile").display()
        ))
        .arg("--no-first-run")
        .arg(format!("--screenshot={}", screenshot.display()))
        .arg(format!(
            "file://{}",
            crate::chat::percent(&page.to_string_lossy())
        ))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .status()
        .await
        .ok()?
        .success();
    if !ok {
        return None;
    }
    tokio::fs::rename(&screenshot, png).await.ok()?;
    Some(png.to_path_buf())
}

/// Rex may not have an attached client when the pane starts. Its pixel dimensions arrive
/// later, and can change when the client changes font size or another device takes over.
fn current_cell_ratio() -> Option<f64> {
    let window = crossterm::terminal::window_size().ok()?;
    if window.width == 0 || window.height == 0 || window.columns == 0 || window.rows == 0 {
        return None;
    }
    Some(
        (window.width as f64 / window.columns as f64) / (window.height as f64 / window.rows as f64),
    )
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
    let reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    if reader.format() != Some(image::ImageFormat::Jpeg) {
        return None;
    }
    reader.into_dimensions().ok()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn png(name: &str, w: u32, h: u32) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("tr3x-test-{}-{name}.png", std::process::id()));
        let mut head = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        head.extend(w.to_be_bytes());
        head.extend(h.to_be_bytes());
        std::fs::write(&path, head).unwrap();
        path
    }

    fn submitted_path(sequence: &str) -> PathBuf {
        let encoded = sequence
            .rsplit(';')
            .next()
            .unwrap()
            .trim_end_matches("\x1b\\");
        PathBuf::from(
            String::from_utf8(
                base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .unwrap(),
            )
            .unwrap(),
        )
    }

    #[tokio::test]
    async fn tall_pictures_keep_aspect_and_use_a_cached_display_derivative() {
        let dir = tempfile::tempdir().unwrap();
        let mut media = Media::new(None);
        media.cache = dir.path().join("cache");
        let (tx, mut rx) = mpsc::unbounded_channel();
        media.listen(tx);
        let tall = dir.path().join("tall.png");
        image::RgbaImage::from_pixel(728, 4000, image::Rgba([10, 20, 30, 255]))
            .save(&tall)
            .unwrap();
        assert!(media.picture(&tall, 90).is_none());
        let ready = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(media.ready(ready));
        let (seq, rows) = media.picture(&tall, 90).unwrap();
        let cols: usize = seq
            .split("c=")
            .nth(1)
            .unwrap()
            .split(',')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let expected_rows =
            cols as f64 * current_cell_ratio().unwrap_or(media.cell_ratio) * 4000.0 / 728.0;
        assert!((rows as f64 - expected_rows).abs() <= 0.5);
        assert!(rows <= 60);
        let derivative = submitted_path(&seq);
        let (w, h) = png_size(&derivative).unwrap();
        assert!(w <= cols as u32 * 16 && h <= 1920);
        assert!((w as f64 / h as f64 - 728.0 / 4000.0).abs() < 0.01);
        assert_eq!(png_size(&tall), Some((728, 4000)), "original stays intact");
        media.free_all();
        assert_eq!(
            submitted_path(&media.picture(&tall, 90).unwrap().0),
            derivative
        );
        assert!(
            rx.try_recv().is_err(),
            "a cached derivative needs no more work"
        );
    }

    #[test]
    fn pictures_past_the_budget_free_the_oldest() {
        let media = Media::new(None);
        // 3 MiB each; the sixth exceeds the 16 MiB budget.
        let picture = png("budget", 1024, 768);
        for id in 1..=5 {
            let seq = media.picture(&picture, 80).unwrap().0;
            assert!(seq.starts_with(&format!("\x1b_Ga=T,i={id},")));
        }
        let sixth = media.picture(&picture, 80).unwrap().0;
        assert!(sixth.starts_with("\x1b_Ga=d,d=I,i=1,q=2\x1b\\\x1b_Ga=T,i=6,"));
        assert_eq!(media.free_all(), FREE_ALL);
        assert!(
            media
                .picture(&picture, 80)
                .unwrap()
                .0
                .starts_with("\x1b_Ga=T,i=7,")
        );
        std::fs::remove_file(picture).unwrap();
    }

    #[test]
    fn snapshots_from_an_old_theme_generation_are_rejected() {
        let mut media = Media::new(None);
        let generation = media.generation;
        let dark = media.dark;
        media.set_dark(!dark);
        media.set_dark(dark);
        assert!(!media.ready(Ready::Snapshot {
            attachment: "html".into(),
            path: Some(PathBuf::from("old.png")),
            generation,
        }));
        assert!(media.snapshots.is_empty());
        assert!(media.ready(Ready::Snapshot {
            attachment: "html".into(),
            path: Some(PathBuf::from("current.png")),
            generation: media.generation,
        }));
        assert_eq!(media.snapshots["html"], PathBuf::from("current.png"));
    }

    #[tokio::test]
    async fn cancelling_a_hung_capture_cleans_its_profile_and_releases_the_slot() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let chrome = dir.path().join("chromium");
        std::fs::write(&chrome, "#!/bin/sh\nexec sleep 60\n").unwrap();
        std::fs::set_permissions(&chrome, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(dir.path().join("page.html"), "<html>test</html>").unwrap();
        let embed = Embed {
            attachment: "page".into(),
            title: "Page".into(),
            height: 800,
        };
        let cache = dir.path().join("cache");
        let result = tokio::time::timeout(
            Duration::from_millis(100),
            capture(
                &chrome,
                dir.path(),
                &cache,
                &embed,
                false,
                &cache.join("result.png"),
            ),
        )
        .await;
        assert!(result.is_err());
        assert!(
            !std::fs::read_dir(&cache)
                .unwrap()
                .flatten()
                .any(|entry| entry.path().is_dir())
        );
        let lock = std::fs::OpenOptions::new()
            .write(true)
            .open(cache.join("capture.lock"))
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
    }

    #[tokio::test]
    async fn a_theme_change_cancels_queued_capture_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let mut media = Media::new(None);
        media.attachments = Some(dir.path().to_path_buf());
        media.cache = dir.path().join("cache");
        media.chrome = Some(PathBuf::from("/nonexistent/chromium"));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let embed = Embed {
            attachment: "missing".into(),
            title: "Missing".into(),
            height: 800,
        };
        media.ask(&embed, tx.clone());
        assert_eq!(media.asked.len(), 1);
        let previous = media.asked["missing"].as_ref().unwrap().abort_handle();
        media.set_dark(!media.dark);
        assert!(media.asked.is_empty());
        tokio::task::yield_now().await;
        assert!(previous.is_finished());
        assert!(rx.try_recv().is_err());
    }
}
