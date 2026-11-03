//! Pictures on their way into a message, as Reply and a new thread hold them: each made ready
//! to send off the input's thread, then shown as a chip over the input with its thumbnail.
//!
//! Ready means T3's web composer's size or less: at most 2000 pixels on the long edge, so a
//! screenshot of part of a Retina screen keeps every pixel. It goes lossless, PNG or WebP,
//! while that's under a megabyte, else as whichever is smallest, JPEG among them when the
//! picture has no transparency.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::codecs::webp::WebPEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageDecoder, ImageReader};

use crate::chat::{DIM, RESET, fit};
use crate::icons::{Icon, Painter};
use crate::paste::Image;

/// The longest edge a picture goes with, as T3's web composer sends it.
const MAX_EDGE: u32 = 2000;
/// Lossless while it's this small.
const LOSSLESS_BYTES: usize = 1024 * 1024;
/// The most T3 takes for one picture.
const MAX_BYTES: usize = 10 * 1024 * 1024;
const JPEG_QUALITY: u8 = 85;
const THUMB: u32 = 64;

/// A picture made ready to send, and its thumbnail when one could be made.
pub struct Ready {
    pub id: u64,
    pub result: Result<(Image, Option<PathBuf>), String>,
}

/// `image` as it goes to T3, and its thumbnail.
fn prepare(image: Image) -> Result<(Image, Option<PathBuf>), String> {
    let unreadable = || format!("Couldn't read {}", image.name);
    // An animation stays as it is.
    if image.mime == "image/gif" {
        let decoded = image::load_from_memory(&image.bytes).map_err(|_| unreadable())?;
        return fits(image).map(|image| (image, thumbnail(&decoded)));
    }
    let bytes = if image.mime == "image/heic" {
        sips_png(&image.bytes).ok_or_else(unreadable)?
    } else {
        image.bytes.clone()
    };
    let mut decoder = ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|_| unreadable())?
        .into_decoder()
        .map_err(|_| unreadable())?;
    let orientation = decoder.orientation().ok();
    let mut decoded = DynamicImage::from_decoder(decoder).map_err(|_| unreadable())?;
    let turned = orientation.is_some_and(|o| o != image::metadata::Orientation::NoTransforms);
    if let Some(o) = orientation {
        decoded.apply_orientation(o);
    }
    let big = decoded.width().max(decoded.height()) > MAX_EDGE;
    if big {
        decoded = decoded.resize(MAX_EDGE, MAX_EDGE, FilterType::Lanczos3);
    }
    let thumb = thumbnail(&decoded);
    let encoded = encode(&decoded);
    // The original, when nothing about it had to change and it's no bigger.
    let keep = !big
        && !turned
        && ["image/png", "image/jpeg", "image/webp"].contains(&image.mime.as_str())
        && encoded
            .as_ref()
            .is_none_or(|(_, b)| image.bytes.len() <= b.len());
    let ready = if keep {
        image
    } else {
        let (mime, bytes) = encoded.ok_or_else(unreadable)?;
        Image {
            name: renamed(&image.name, mime),
            mime: mime.to_string(),
            bytes,
        }
    };
    fits(ready).map(|image| (image, thumb))
}

fn fits(image: Image) -> Result<Image, String> {
    if image.bytes.len() > MAX_BYTES {
        return Err(format!("{} is over 10 MB, even made smaller", image.name));
    }
    Ok(image)
}

/// The smaller lossless encoding while it's small enough, else the smallest of all.
fn encode(image: &DynamicImage) -> Option<(&'static str, Vec<u8>)> {
    let opaque = !image.color().has_alpha() || image.to_rgba8().pixels().all(|p| p[3] == 255);
    let image = if opaque {
        DynamicImage::ImageRgb8(image.to_rgb8())
    } else {
        DynamicImage::ImageRgba8(image.to_rgba8())
    };
    let mut png = Vec::new();
    image
        .write_with_encoder(PngEncoder::new_with_quality(
            &mut png,
            CompressionType::Best,
            PngFilter::Adaptive,
        ))
        .ok()?;
    let mut webp = Vec::new();
    let webp = image
        .write_with_encoder(WebPEncoder::new_lossless(&mut webp))
        .ok()
        .map(|_| webp);
    let lossless = match webp {
        Some(webp) if webp.len() < png.len() => ("image/webp", webp),
        _ => ("image/png", png),
    };
    if lossless.1.len() <= LOSSLESS_BYTES || !opaque {
        return Some(lossless);
    }
    let mut jpeg = Vec::new();
    image
        .write_with_encoder(JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY))
        .ok()?;
    Some(if jpeg.len() < lossless.1.len() {
        ("image/jpeg", jpeg)
    } else {
        lossless
    })
}

/// `name` with the extension of `mime`.
fn renamed(name: &str, mime: &str) -> String {
    let stem = Path::new(name)
        .file_stem()
        .map_or("image".into(), |s| s.to_string_lossy());
    let ext = match mime {
        "image/jpeg" => "jpg",
        other => other.trim_start_matches("image/"),
    };
    format!("{stem}.{ext}")
}

/// The Mac's `sips`, or ImageMagick elsewhere, for the HEIC the decoder can't read.
fn sips_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let dir = std::env::temp_dir().join(format!("tr3x-paste-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).ok()?;
    let (src, dst) = (dir.join("in.heic"), dir.join("out.png"));
    let done = std::fs::write(&src, bytes).ok().and_then(|_| {
        if cfg!(target_os = "macos") {
            std::process::Command::new("/usr/bin/sips")
                .args(["-s", "format", "png"])
                .arg(&src)
                .arg("--out")
                .arg(&dst)
                .output()
        } else {
            std::process::Command::new("magick")
                .arg(&src)
                .arg(&dst)
                .output()
        }
        .ok()
        .filter(|o| o.status.success())
        .and_then(|_| std::fs::read(&dst).ok())
    });
    let _ = std::fs::remove_dir_all(&dir);
    done
}

/// A square PNG of the picture's middle, for its chip: two cells are about square.
fn thumbnail(image: &DynamicImage) -> Option<PathBuf> {
    let side = image.width().min(image.height());
    if side == 0 {
        return None;
    }
    let (x, y) = ((image.width() - side) / 2, (image.height() - side) / 2);
    let thumb = image
        .crop_imm(x, y, side, side)
        .resize_exact(THUMB, THUMB, FilterType::Triangle);
    let dir = std::env::temp_dir().join("tr3x-thumbs");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{}.png", uuid::Uuid::new_v4()));
    thumb.save(&path).ok()?;
    Some(path)
}

struct Held {
    id: u64,
    name: String,
    /// `None` while it's being made ready.
    image: Option<Image>,
    thumb: Option<PathBuf>,
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Some(thumb) = &self.thumb {
            let _ = std::fs::remove_file(thumb);
        }
    }
}

/// The pictures the next message takes.
#[derive(Default)]
pub struct Tray {
    held: Vec<Held>,
    next: u64,
}

impl Tray {
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// Whether a picture is still being made ready.
    pub fn preparing(&self) -> bool {
        self.held.iter().any(|h| h.image.is_none())
    }

    /// Makes each picture ready off this thread; `done` gets each one as it is.
    pub fn add<F>(&mut self, images: Vec<Image>, done: F)
    where
        F: Fn(Ready) + Clone + Send + 'static,
    {
        for image in images {
            self.next += 1;
            let id = self.next;
            self.held.push(Held {
                id,
                name: image.name.clone(),
                image: None,
                thumb: None,
            });
            let done = done.clone();
            tokio::task::spawn_blocking(move || {
                done(Ready {
                    id,
                    result: prepare(image),
                })
            });
        }
    }

    /// Takes a picture that's ready. Returns why it couldn't be made ready, and drops it then.
    pub fn ready(&mut self, ready: Ready) -> Option<String> {
        let Some(at) = self.held.iter().position(|h| h.id == ready.id) else {
            if let Ok((_, Some(thumb))) = ready.result {
                let _ = std::fs::remove_file(thumb);
            }
            return None;
        };
        match ready.result {
            Ok((image, thumb)) => {
                let held = &mut self.held[at];
                held.name = image.name.clone();
                held.image = Some(image);
                held.thumb = thumb;
                None
            }
            Err(why) => {
                self.held.remove(at);
                Some(why)
            }
        }
    }

    pub fn remove(&mut self, i: usize) {
        if i < self.held.len() {
            self.held.remove(i);
        }
    }

    pub fn pop(&mut self) {
        self.held.pop();
    }

    pub fn clear(&mut self) {
        self.held.clear();
    }

    /// Every picture, once each is ready, leaving the tray empty.
    pub fn take(&mut self) -> Vec<Image> {
        std::mem::take(&mut self.held)
            .into_iter()
            .filter_map(|mut h| h.image.take())
            .collect()
    }

    /// Clones of the pictures that are ready, leaving them in the tray.
    pub fn images(&self) -> Vec<Image> {
        self.held.iter().filter_map(|h| h.image.clone()).collect()
    }

    /// The row of chips: each picture's thumbnail and name, then its ✕, whose column comes
    /// back for each picture shown, in order, and the row's width. Columns start at `left + 1`.
    pub fn row(
        &self,
        painter: &mut Painter,
        left: usize,
        width: usize,
    ) -> (String, Vec<usize>, usize) {
        let (mut line, mut spans, mut col) = (String::new(), Vec::new(), left + 1);
        for (i, held) in self.held.iter().enumerate() {
            let name = fit(&held.name, 24);
            let w = unicode_width::UnicodeWidthStr::width(name.as_str());
            // Three between chips, then two for the thumbnail, a space, the name, a space and
            // the ✕, and room left for what doesn't fit.
            if i > 0 && col + 3 + 2 + 1 + w + 2 + 4 > left + 1 + width {
                let rest = format!("  +{}", self.held.len() - i);
                line.push_str(&format!("{DIM}{rest}{RESET}"));
                col += rest.len();
                break;
            }
            let icon = match &held.thumb {
                Some(path) => Icon::Picture(path.clone()),
                None => Icon::Text("▣".into(), 8),
            };
            if i > 0 {
                line.push_str("   ");
                col += 3;
            }
            line.push_str(&painter.draw(&icon));
            line.push_str(&format!(" {name} {DIM}✕{RESET}"));
            let x = col + 2 + 1 + w + 1;
            spans.push(x);
            col = x + 1;
        }
        (line, spans, col - (left + 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screenshot(w: u32, h: u32, alpha: bool) -> Image {
        // Flat UI with some text-like noise, as a screenshot is.
        let mut img = image::RgbaImage::from_pixel(w, h, image::Rgba([245, 245, 247, 255]));
        for (x, y, p) in img.enumerate_pixels_mut() {
            if (x / 7 + y / 13) % 5 == 0 && (x * 31 + y * 17) % 3 == 0 {
                *p = image::Rgba([(x % 255) as u8, (y % 255) as u8, 40, 255]);
            }
            if alpha && x < 10 {
                p[3] = 0;
            }
        }
        let mut bytes = Vec::new();
        DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        Image {
            name: "Screenshot.png".into(),
            mime: "image/png".into(),
            bytes,
        }
    }

    #[test]
    fn a_big_screenshot_shrinks_to_the_long_edge() {
        let (image, thumb) = prepare(screenshot(3024, 1964, false)).unwrap();
        let decoded = image::load_from_memory(&image.bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (2000, 1299));
        assert!(image.bytes.len() <= MAX_BYTES);
        assert!(image.name.starts_with("Screenshot."));
        let thumb = thumb.unwrap();
        let t = image::open(&thumb).unwrap();
        assert_eq!((t.width(), t.height()), (THUMB, THUMB));
        let _ = std::fs::remove_file(thumb);
    }

    #[test]
    fn a_small_one_keeps_its_pixels_and_transparency() {
        let source = screenshot(1200, 800, true);
        let (image, thumb) = prepare(source.clone()).unwrap();
        let decoded = image::load_from_memory(&image.bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (1200, 800));
        assert!(decoded.color().has_alpha(), "{}", image.mime);
        assert_ne!(image.mime, "image/jpeg");
        assert!(image.bytes.len() <= source.bytes.len());
        let _ = thumb.map(std::fs::remove_file);
    }

    #[test]
    fn junk_says_so() {
        let junk = Image {
            name: "x.png".into(),
            mime: "image/png".into(),
            bytes: b"not a picture".to_vec(),
        };
        assert_eq!(prepare(junk).err(), Some("Couldn't read x.png".into()));
        assert_eq!(renamed("Shot 1.png", "image/jpeg"), "Shot 1.jpg");
    }
}
