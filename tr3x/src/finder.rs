//! **New T3 Thread Here** in Finder's Quick Actions, for a folder: it asks what the thread
//! should do, then runs `tr3x new --prompt - --folder <folder>`. The thread goes to the project
//! that holds the folder, which becomes one when none does, and its tab opens in Rex. A macOS
//! Service (an Automator workflow in `~/Library/Services`), so it also shows under
//! Services in the folder's menu, and can take a keyboard shortcut in System Settings.

use std::path::{Path, PathBuf};

use anyhow::Result;

const INFO: &str = include_str!("../assets/finder/Info.plist");
const DOCUMENT: &str = include_str!("../assets/finder/document.wflow");

/// Where the Quick Action lives.
pub fn path() -> PathBuf {
    crate::install::home().join("Library/Services/New T3 Thread.workflow")
}

/// The shell script the workflow runs with the folder as `$1`.
fn script(bin: &Path) -> String {
    let bin = bin.to_string_lossy().replace('\'', r"'\''");
    format!(
        r#"dir="$1"
[ -d "$dir" ] || exit 0
msg=$(osascript -e 'on run argv' -e 'text returned of (display dialog "New thread in " & (item 1 of argv) default answer "" buttons {{"Cancel", "Start"}} default button "Start" with title "T3 Code")' -e 'end run' "${{dir:t}}") || exit 0
[ -n "$msg" ] || exit 0
printf '%s' "$msg" | '{bin}' new --prompt - --folder "$dir""#
    )
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The workflow's `document.wflow` for `bin`.
fn document(bin: &Path) -> String {
    DOCUMENT.replace("@COMMAND@", &xml_escape(&script(bin)))
}

/// Writes the Quick Action for `bin`. Returns whether it changed.
pub fn install(bin: &Path) -> Result<bool> {
    let dir = path().join("Contents");
    let doc = document(bin);
    if std::fs::read_to_string(dir.join("document.wflow"))
        .ok()
        .as_deref()
        == Some(&doc)
        && std::fs::read_to_string(dir.join("Info.plist"))
            .ok()
            .as_deref()
            == Some(INFO)
    {
        return Ok(false);
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("Info.plist"), INFO)?;
    std::fs::write(dir.join("document.wflow"), doc)?;
    Ok(true)
}

/// Removes the Quick Action. Returns whether there was one.
pub fn remove() -> bool {
    std::fs::remove_dir_all(path()).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_workflow_runs_tr3x_with_the_folder() {
        let doc = document(Path::new("/Users/me/.local/bin/tr3x"));
        assert!(!doc.contains("@COMMAND@"));
        assert!(doc.contains(
            "printf '%s' \"$msg\" | '/Users/me/.local/bin/tr3x' new --prompt - --folder \"$dir\""
        ));
        assert!(doc.contains("\"New thread in \" &amp; (item 1 of argv)"));
        assert!(INFO.contains("<string>New T3 Thread Here</string>"));
        assert!(INFO.contains("<string>public.folder</string>"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn both_files_are_property_lists() {
        let dir = tempfile::tempdir().unwrap();
        let doc = dir.path().join("document.wflow");
        let info = dir.path().join("Info.plist");
        std::fs::write(&doc, document(Path::new("/opt/a&b/<tr3x>"))).unwrap();
        std::fs::write(&info, INFO).unwrap();
        let lint = std::process::Command::new("/usr/bin/plutil")
            .arg("-lint")
            .args([&doc, &info])
            .output()
            .unwrap();
        assert!(
            lint.status.success(),
            "{}",
            String::from_utf8_lossy(&lint.stdout)
        );
    }
}
