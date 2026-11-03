//! What differs between a Mac and a Linux machine: where caches live, which release to
//! download, and where a headless Chromium might be. Everything else tr3x does is the same.

use std::path::PathBuf;

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// The folder caches go in: `~/Library/Caches` on a Mac, `$XDG_CACHE_HOME` or `~/.cache` on
/// Linux.
pub fn caches() -> PathBuf {
    if cfg!(target_os = "macos") {
        return home().join("Library/Caches");
    }
    match std::env::var_os("XDG_CACHE_HOME").filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => home().join(".cache"),
    }
}

/// tr3x's own cache folder.
pub fn cache() -> PathBuf {
    caches().join("tr3x")
}

/// Where Playwright keeps the browsers it downloads.
pub fn playwright() -> PathBuf {
    caches().join("ms-playwright")
}

/// The release folder and tarball for this machine: `tr3x-macos-universal`,
/// `tr3x-linux-x86_64`, `tr3x-linux-aarch64`.
pub fn release_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "tr3x-macos-universal"
    } else if cfg!(target_arch = "aarch64") {
        "tr3x-linux-aarch64"
    } else {
        "tr3x-linux-x86_64"
    }
}

/// Browsers a Linux machine has on its `PATH`, or a Mac has in /Applications.
pub fn browsers() -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        return [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
        ]
        .map(PathBuf::from)
        .to_vec();
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    [
        "chromium",
        "chromium-browser",
        "google-chrome-stable",
        "google-chrome",
        "chrome",
    ]
    .iter()
    .flat_map(|name| dirs.iter().map(move |d| d.join(name)))
    .collect()
}

/// Whether this is a Mac, where Rex is an app with preferences, themes and a launchd agent to
/// retire.
pub const MAC: bool = cfg!(target_os = "macos");
