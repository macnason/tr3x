//! Pictures in Reply. With paste events on (mode 5522), Rex doesn't paste: it says what the
//! clipboard holds, as a kitty clipboard protocol MIME list (OSC 5522), with a password that
//! reads it once without asking. The input asks for a picture when there is one, else the
//! text, and the reply comes back in pieces. Text pastes go the same way, so they still work.
//! The clipboard is the one on the device the paste came from.
//!
//! A pasted path to a picture, as dropping a file pastes it, attaches the picture too. What
//! comes out is the picture as it was; `picture` makes it ready to send.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

pub const ON: &str = "\x1b[?5522h";
pub const OFF: &str = "\x1b[?5522l";

/// The pictures T3 hands a provider, best first.
const PICTURES: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];
/// Pictures that become one of those before they go.
const CONVERTS: [&str; 2] = ["image/tiff", "image/heic"];
/// The most of a picture worth reading in, before it's made smaller.
const MAX_SOURCE_BYTES: usize = 50 * 1024 * 1024;
/// `Paste event`, the name a paste's password goes with.
const PASTE_EVENT: &str = "UGFzdGUgZXZlbnQ=";

#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub name: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

impl Image {
    /// What `assets.persistChatAttachments` takes.
    pub fn upload(&self) -> Value {
        json!({
            "type": "image",
            "name": self.name,
            "mimeType": self.mime,
            "sizeBytes": self.bytes.len(),
            "dataUrl": format!("data:{};base64,{}", self.mime, STANDARD.encode(&self.bytes)),
        })
    }

    fn new(name: String, mime: &str, bytes: Vec<u8>) -> Result<Image, String> {
        if bytes.len() > MAX_SOURCE_BYTES {
            return Err(format!("{name} is over 50 MB"));
        }
        Ok(Image {
            name,
            mime: mime.to_string(),
            bytes,
        })
    }
}

/// What a run of clipboard replies came to.
#[derive(Debug, PartialEq)]
pub enum Got {
    /// A paste: what the clipboard holds, and the request that reads what Reply wants of it.
    Offer(String),
    Text(String),
    Images(Vec<Image>),
    /// Nothing to attach or insert, with what to say about it, if anything.
    Nothing(Option<String>),
}

/// Puts a reply's packets back together.
#[derive(Default)]
pub struct Reader {
    /// The reply so far: each MIME type's base64 chunks, in order.
    data: Vec<(String, Vec<String>)>,
    /// The password and location a paste came with, for the read that follows it.
    meta: String,
    reading: bool,
}

impl Reader {
    /// Takes one packet, as the input filter hands it over, and returns what the reply came to
    /// once it ends.
    pub fn feed(&mut self, packet: &str) -> Option<Got> {
        let (meta, payload) = packet.split_once(';').unwrap_or((packet, ""));
        let keys: HashMap<&str, &str> = meta
            .split(':')
            .filter_map(|kv| kv.split_once('='))
            .collect();
        if keys.get("type") != Some(&"read") {
            return None;
        }
        match keys.get("status").copied() {
            Some("OK") => {
                self.data.clear();
                self.reading = true;
                self.meta = ["loc", "pw"]
                    .iter()
                    .filter_map(|k| keys.get(k).map(|v| format!(":{k}={v}")))
                    .collect();
            }
            Some("DATA") if self.reading => {
                let mime = keys.get("mime").and_then(|m| decode(m)).unwrap_or_default();
                let mime = String::from_utf8_lossy(&mime).into_owned();
                match self.data.last_mut() {
                    Some((last, chunks)) if *last == mime => chunks.push(payload.to_string()),
                    _ => self.data.push((mime, vec![payload.to_string()])),
                }
            }
            Some("DONE") if self.reading => {
                self.reading = false;
                return Some(self.done());
            }
            Some("DATA" | "DONE") => {}
            Some(code) => {
                self.reading = false;
                return Some(Got::Nothing(Some(match code {
                    "EPERM" => "Rex didn't allow reading the clipboard".into(),
                    _ => format!("Couldn't read the clipboard ({code})"),
                })));
            }
            None => {}
        }
        None
    }

    fn done(&mut self) -> Got {
        let data: Vec<(String, Vec<u8>)> = std::mem::take(&mut self.data)
            .into_iter()
            .map(|(mime, chunks)| (mime, join(&chunks)))
            .collect();
        if let Some((_, list)) = data.iter().find(|(mime, _)| mime == ".") {
            let list = String::from_utf8_lossy(list);
            let mimes: Vec<&str> = list.split_ascii_whitespace().collect();
            return Got::Offer(request(&want(&mimes), &self.meta));
        }
        let get = |want: &str| {
            data.iter()
                .find(|(m, _)| m == want)
                .map(|(_, b)| b.as_slice())
        };
        if let Some(list) = get("text/uri-list") {
            let paths: Vec<PathBuf> = String::from_utf8_lossy(list)
                .lines()
                .filter_map(|l| l.trim().strip_prefix("file://"))
                .map(|p| PathBuf::from(unescape_url(p)))
                .collect();
            if let Some(got) = files(&paths) {
                return got;
            }
        }
        if let Some((mime, bytes)) = data
            .iter()
            .find(|(m, _)| PICTURES.contains(&m.as_str()) || CONVERTS.contains(&m.as_str()))
        {
            let ext = mime.trim_start_matches("image/").replace("jpeg", "jpg");
            return match Image::new(format!("image.{ext}"), mime, bytes.clone()) {
                Ok(image) => Got::Images(vec![image]),
                Err(e) => Got::Nothing(Some(e)),
            };
        }
        match data.iter().find(|(m, _)| m.starts_with("text/plain")) {
            Some((_, text)) => Got::Text(String::from_utf8_lossy(text).into_owned()),
            None => Got::Nothing(None),
        }
    }
}

/// The types to read from a clipboard that holds `mimes`: the files, else a picture, else
/// the text.
fn want(mimes: &[&str]) -> Vec<String> {
    let text: Vec<String> = mimes
        .iter()
        .filter(|m| m.starts_with("text/plain"))
        .map(|m| m.to_string())
        .collect();
    let text = if text.is_empty() {
        vec!["text/plain".to_string()]
    } else {
        text
    };
    // Files come with their icon as a picture, so they go first.
    if mimes.contains(&"text/uri-list") {
        return std::iter::once("text/uri-list".to_string())
            .chain(text)
            .collect();
    }
    match PICTURES.iter().chain(&CONVERTS).find(|p| mimes.contains(p)) {
        Some(picture) => vec![picture.to_string()],
        None => text,
    }
}

/// The read request for `mimes`, with the paste's password and location.
fn request(mimes: &[String], meta: &str) -> String {
    let name = if meta.contains(":pw=") {
        format!(":name={PASTE_EVENT}")
    } else {
        String::new()
    };
    format!(
        "\x1b]5522;type=read{meta}{name};{}\x1b\\",
        STANDARD.encode(mimes.join(" "))
    )
}

/// A ⌘V that reached Reply as a key, which Rex sends when it didn't paste: a clipboard its
/// paste has no use for, or paste events (mode 5522) on a pane another device is typing in.
/// From this Mac, the clipboard is read here. From another device, Rex is asked to paste
/// again with paste events off, so it uploads the picture and pastes its path.
pub async fn by_hand() -> Got {
    let rex = crate::rex::Rex::find();
    let owner = match std::env::var("REX_BLOCK") {
        Ok(block) => rex.owner(&block).await.ok().flatten(),
        Err(_) => None,
    };
    match owner {
        Some((client, false)) => replay(&rex, &client).await,
        _ => here().await,
    }
}

async fn replay(rex: &crate::rex::Rex, client: &str) -> Got {
    let events = crate::scheme::filtering();
    if events {
        crate::term::out(OFF);
    }
    let done = rex.app_action(client, "pane.paste").await;
    if events {
        // Rex reads the mode as the paste starts; give it the moment it takes to get there.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        crate::term::out(ON);
    }
    match done {
        Ok(_) => Got::Nothing(None),
        Err(e) => Got::Nothing(Some(format!("Couldn't paste: {e:#}"))),
    }
}

/// What this Mac's clipboard holds: its files, else a picture, else the text.
#[cfg(target_os = "macos")]
async fn here() -> Got {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("tr3x-clipboard-{}-{n}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Got::Nothing(Some(format!("Couldn't read the clipboard: {e}")));
    }
    let out = tokio::process::Command::new("/usr/bin/osascript")
        .args(["-l", "JavaScript", "-e", include_str!("clipboard.js")])
        .arg(&dir)
        .stdin(std::process::Stdio::null())
        .output()
        .await;
    let got = match out {
        Ok(out) if out.status.success() => read_by_hand(&out.stdout),
        _ => Got::Nothing(Some("Couldn't read the clipboard".into())),
    };
    let _ = std::fs::remove_dir_all(&dir);
    got
}

#[cfg(not(target_os = "macos"))]
async fn here() -> Got {
    Got::Nothing(Some(
        "Couldn't read the clipboard. Drag the picture in instead.".into(),
    ))
}

/// What `clipboard.js` printed: `{files}`, `{text}` or nothing.
fn read_by_hand(json: &[u8]) -> Got {
    let said: Value = serde_json::from_slice(json).unwrap_or_default();
    let paths: Vec<PathBuf> = said["files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p.as_str().map(PathBuf::from))
        .collect();
    if !paths.is_empty() {
        return files(&paths).unwrap_or_else(|| {
            let escaped: Vec<String> = paths
                .iter()
                .map(|p| p.to_string_lossy().replace(' ', "\\ "))
                .collect();
            Got::Text(escaped.join(" "))
        });
    }
    match said["text"].as_str() {
        Some(text) => Got::Text(text.to_string()),
        None => Got::Nothing(None),
    }
}

/// Pasted text that is only paths to pictures, as dropping files pastes them: those pictures.
pub fn dropped(text: &str) -> Option<Got> {
    let paths = split_paths(text.trim())?;
    files(&paths.iter().map(PathBuf::from).collect::<Vec<_>>())
}

/// The pictures at `paths`, when every one is a picture.
pub(crate) fn files(paths: &[PathBuf]) -> Option<Got> {
    if paths.is_empty()
        || !paths
            .iter()
            .all(|p| p.is_absolute() && mime_of(p).is_some())
    {
        return None;
    }
    let mut images = Vec::new();
    for path in paths {
        let name = path.file_name()?.to_string_lossy().into_owned();
        let bytes = std::fs::read(path).ok()?;
        match Image::new(name, mime_of(path)?, bytes) {
            Ok(image) => images.push(image),
            Err(e) => return Some(Got::Nothing(Some(e))),
        }
    }
    Some(Got::Images(images))
}

pub(crate) fn mime_of(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "tif" | "tiff" => "image/tiff",
        "heic" => "image/heic",
        _ => return None,
    })
}

/// Paths as a terminal pastes dropped files: separated by spaces, each escaped with
/// backslashes or quoted. `None` for text that doesn't parse that way.
fn split_paths(text: &str) -> Option<Vec<String>> {
    if text.is_empty() || text.contains('\n') {
        return None;
    }
    let (mut paths, mut cur, mut quote) = (Vec::new(), String::new(), None);
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match (c, quote) {
            ('\\', None) => cur.push(chars.next()?),
            ('\'' | '"', None) => quote = Some(c),
            (c, Some(q)) if c == q => quote = None,
            (' ', None) => {
                if !cur.is_empty() {
                    paths.push(std::mem::take(&mut cur));
                }
            }
            (c, _) => cur.push(c),
        }
    }
    if quote.is_some() {
        return None;
    }
    if !cur.is_empty() {
        paths.push(cur);
    }
    Some(paths)
}

fn unescape_url(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = path
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn decode(chunk: &str) -> Option<Vec<u8>> {
    STANDARD.decode(chunk).ok()
}

/// A reply's chunks as bytes. Each chunk ends padded, so each decodes alone; a terminal that
/// splits one encoding across packets is read whole.
fn join(chunks: &[String]) -> Vec<u8> {
    chunks
        .iter()
        .map(|c| decode(c))
        .collect::<Option<Vec<_>>>()
        .map(|parts| parts.concat())
        .or_else(|| decode(&chunks.concat()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(s: &str) -> String {
        STANDARD.encode(s)
    }

    #[test]
    fn a_paste_reads_the_picture() {
        let mut r = Reader::default();
        let pw = b64("secret");
        assert_eq!(r.feed(&format!("type=read:status=OK:pw={pw}")), None);
        let list = format!(
            "type=read:status=DATA:mime=Lg==:pw={pw};{}",
            b64("text/plain image/tiff image/png\n")
        );
        assert_eq!(r.feed(&list), None);
        let Some(Got::Offer(request)) = r.feed(&format!("type=read:status=DONE:pw={pw}")) else {
            panic!("no offer");
        };
        assert_eq!(
            request,
            format!(
                "\x1b]5522;type=read:pw={pw}:name={PASTE_EVENT};{}\x1b\\",
                b64("image/png")
            )
        );
        // The picture, in two chunks.
        r.feed("type=read:status=OK");
        let png = b64("image/png");
        r.feed(&format!(
            "type=read:status=DATA:mime={png};{}",
            STANDARD.encode(b"\x89PN")
        ));
        r.feed(&format!(
            "type=read:status=DATA:mime={png};{}",
            STANDARD.encode(b"G")
        ));
        let got = r.feed("type=read:status=DONE");
        assert_eq!(
            got,
            Some(Got::Images(vec![Image {
                name: "image.png".into(),
                mime: "image/png".into(),
                bytes: b"\x89PNG".to_vec(),
            }]))
        );
    }

    #[test]
    fn text_still_pastes() {
        assert_eq!(
            want(&["text/plain;charset=utf-8", "text/html"]),
            ["text/plain;charset=utf-8"]
        );
        assert_eq!(want(&["public.data"]), ["text/plain"]);
        assert_eq!(
            want(&["image/png", "text/uri-list", "text/plain"]),
            ["text/uri-list", "text/plain"]
        );
        let mut r = Reader::default();
        r.feed("type=read:status=OK");
        r.feed(&format!(
            "type=read:status=DATA:mime={};{}",
            b64("text/plain"),
            b64("hi there")
        ));
        assert_eq!(
            r.feed("type=read:status=DONE"),
            Some(Got::Text("hi there".into()))
        );
        r.feed("type=read:status=EPERM");
        assert!(!r.reading);
    }

    #[test]
    fn by_hand_reads() {
        assert_eq!(read_by_hand(br#"{"text":"hi"}"#), Got::Text("hi".into()));
        assert_eq!(read_by_hand(b"{}"), Got::Nothing(None));
        assert_eq!(
            read_by_hand(br#"{"files":["/a/My Notes.txt"]}"#),
            Got::Text(r"/a/My\ Notes.txt".into())
        );
        let dir = std::env::temp_dir().join(format!("tr3x-by-hand-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let shot = dir.join("image.webp");
        std::fs::write(&shot, b"RIFF").unwrap();
        let json = serde_json::json!({ "files": [shot] }).to_string();
        let Got::Images(images) = read_by_hand(json.as_bytes()) else {
            panic!("not attached");
        };
        assert_eq!(images[0].mime, "image/webp");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn dropped_paths() {
        assert_eq!(
            split_paths(r"/a/My\ Shot.png '/b/c d.jpg'"),
            Some(vec!["/a/My Shot.png".into(), "/b/c d.jpg".into()])
        );
        assert_eq!(split_paths("'/open"), None);
        let dir = std::env::temp_dir().join(format!("tr3x-paste-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let shot = dir.join("Shot 1.png");
        std::fs::write(&shot, b"\x89PNG").unwrap();
        let pasted = shot.to_string_lossy().replace(' ', "\\ ");
        let Some(Got::Images(images)) = dropped(&pasted) else {
            panic!("not attached");
        };
        assert_eq!(
            (images[0].name.as_str(), images[0].mime.as_str()),
            ("Shot 1.png", "image/png")
        );
        // Words, and paths that aren't all pictures, stay text.
        assert_eq!(dropped("fix the logo"), None);
        assert_eq!(dropped(&format!("{pasted} /etc/hosts")), None);
        assert_eq!(unescape_url("/a/Shot%201.png"), "/a/Shot 1.png");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rex_upload_paste() {
        // What Rex 1853 pastes on the host after a drop or paste from another device: the
        // upload's path, shell-escaped, then a space.
        let dir = std::env::temp_dir()
            .join(format!("tr3x-upload-test-{}", std::process::id()))
            .join("uploads/1-0/block/upload");
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("Screen Shot 1.webp"), dir.join("it's.png"));
        std::fs::write(&a, b"RIFF").unwrap();
        std::fs::write(&b, b"\x89PNG").unwrap();
        let escape = |p: &Path| p.to_string_lossy().replace(' ', "\\ ").replace('\'', "\\'");
        let pasted = format!("{} {} ", escape(&a), escape(&b));
        let Some(Got::Images(images)) = dropped(&pasted) else {
            panic!("not attached: {pasted}");
        };
        let names: Vec<_> = images
            .iter()
            .map(|i| (i.name.as_str(), i.mime.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("Screen Shot 1.webp", "image/webp"),
                ("it's.png", "image/png")
            ]
        );
        let _ = std::fs::remove_dir_all(dir.ancestors().nth(4).unwrap());
    }
}
