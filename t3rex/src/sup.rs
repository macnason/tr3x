//! sup (github.com/superultrainc/sup) is what the pull request drawer shows: reviews, checks,
//! files, and merging from the keyboard. It's optional: t3rex never installs it on its own. A
//! drawer that opens without it offers to, and Enter puts it in ~/.local/bin, from sup's own
//! releases, checked against the release's checksums.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

const RELEASES: &str = "https://github.com/superultrainc/sup/releases/latest/download";

/// Where t3rex puts sup.
pub fn target() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".local/bin/sup")
}

/// sup's release for this machine: `sup_darwin_arm64.tar.gz`, `sup_linux_amd64.tar.gz`…
fn asset() -> Option<String> {
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        return None;
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86_64") {
        "amd64"
    } else {
        return None;
    };
    Some(format!("sup_{os}_{arch}.tar.gz"))
}

/// Downloads sup's latest release for this machine, checks it against the release's
/// checksums, and puts it at `target()`.
pub async fn install() -> Result<PathBuf> {
    let asset = asset().context("sup has no build for this machine")?;
    let dir = crate::state::dir().join("sup-download");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let result = fetch_into(&asset, &dir).await;
    let _ = std::fs::remove_dir_all(&dir);
    result
}

async fn fetch_into(asset: &str, dir: &Path) -> Result<PathBuf> {
    let tarball = dir.join(asset);
    let tarball_s = tarball.to_string_lossy().into_owned();
    crate::update::curl(&["-o", &tarball_s, &format!("{RELEASES}/{asset}")])
        .await
        .context("couldn't download sup")?;
    let sums = crate::update::curl(&[&format!("{RELEASES}/checksums.txt")])
        .await
        .context("couldn't download sup's checksums")?;
    let want = checksum_for(&String::from_utf8_lossy(&sums), asset)
        .with_context(|| format!("sup's checksums don't list {asset}"))?;
    if crate::update::sha256(&tarball).await? != want {
        bail!("sup's download doesn't match its checksum");
    }
    let status = tokio::process::Command::new("tar")
        .arg("-xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(dir)
        .arg("sup")
        .status()
        .await?;
    let unpacked = dir.join("sup");
    if !status.success() || !unpacked.is_file() {
        bail!("couldn't unpack sup");
    }
    let target = target();
    std::fs::create_dir_all(target.parent().context("no home folder")?)?;
    // Beside it and renamed, so a running sup keeps its old binary.
    let tmp = target.with_extension("new");
    std::fs::copy(&unpacked, &tmp)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, &target)?;
    if crate::platform::MAC {
        // A download can be quarantined; this one was asked for.
        let _ = std::process::Command::new("/usr/bin/xattr")
            .args(["-d", "com.apple.quarantine"])
            .arg(&target)
            .output();
    }
    Ok(target)
}

/// The SHA-256 a `checksums.txt` gives `asset`.
fn checksum_for(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let sum = parts.next()?;
        (parts.next()?.trim_start_matches('*') == asset && sum.len() == 64)
            .then(|| sum.to_lowercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checksum_for_this_asset() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let sums = format!(
            "{a}  sup_darwin_arm64.tar.gz\n{b}  sup_linux_amd64.tar.gz\nshort  sup_linux_arm64.tar.gz\n"
        );
        assert_eq!(
            checksum_for(&sums, "sup_linux_amd64.tar.gz"),
            Some("b".repeat(64))
        );
        assert_eq!(checksum_for(&sums, "sup_linux_arm64.tar.gz"), None);
        assert_eq!(checksum_for(&sums, "sup_windows.zip"), None);
    }

    /// Fetches sup's real release into a throwaway home. It needs the network, so it runs
    /// only on request: `cargo test -p t3rex installs_sup -- --ignored --test-threads=1`.
    #[tokio::test]
    #[ignore]
    async fn installs_sup_from_its_release() {
        let home = tempfile::tempdir().unwrap();
        // SAFETY: this test runs on its own, with --test-threads=1.
        unsafe {
            std::env::set_var("HOME", home.path());
            std::env::set_var("T3REX_STATE_DIR", home.path().join("state"));
        }
        let at = install().await.unwrap();
        assert_eq!(at, home.path().join(".local/bin/sup"));
        assert!(std::fs::metadata(&at).unwrap().len() > 1_000_000);
        assert!(!home.path().join("state/sup-download").exists());
    }

    #[test]
    fn there_is_a_release_for_this_machine() {
        assert!(asset().is_some_and(|a| a.starts_with("sup_") && a.ends_with(".tar.gz")));
    }
}
