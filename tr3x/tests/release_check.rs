//! `scripts/release-check` against archives built the way release.yml builds them: what it
//! lets through is what the release workflow goes on to stage and publish.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const NAMES: [&str; 3] = [
    "tr3x-macos-universal",
    "tr3x-linux-x86_64",
    "tr3x-linux-aarch64",
];
const FILES: [&str; 5] = [
    "tr3x",
    "README.md",
    "LICENSE",
    "LICENSE-t3client",
    "THIRD-PARTY-LICENSES.txt",
];

struct Release {
    dir: tempfile::TempDir,
}

impl Release {
    /// Every platform's archive, complete, with its checksum.
    fn new() -> Release {
        let release = Release {
            dir: tempfile::tempdir().unwrap(),
        };
        for name in NAMES {
            release.pack(name, &FILES, "Copyright (c) 2026 Krish Garg");
        }
        release
    }

    fn dist(&self) -> PathBuf {
        self.dir.path().join("dist")
    }

    /// Replaces `name`'s archive with one holding `files`, the upstream notice saying `notice`.
    fn pack(&self, name: &str, files: &[&str], notice: &str) {
        let src = self.dir.path().join("src");
        let _ = std::fs::remove_dir_all(src.join(name));
        std::fs::create_dir_all(src.join(name)).unwrap();
        std::fs::create_dir_all(self.dist()).unwrap();
        for file in files {
            let text = if *file == "LICENSE-t3client" {
                notice
            } else {
                file
            };
            std::fs::write(src.join(name).join(file), text).unwrap();
        }
        let script = format!(
            "tar -czf dist/{name}.tar.gz -C src {name} && cd dist && \
             (sha256sum {name}.tar.gz 2>/dev/null || shasum -a 256 {name}.tar.gz) > {name}.tar.gz.sha256"
        );
        let status = Command::new("sh")
            .args(["-c", &script])
            .current_dir(self.dir.path())
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn check(&self) -> Output {
        run(&self.dist())
    }
}

fn run(dist: &Path) -> Output {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/release-check");
    Command::new("sh").arg(script).arg(dist).output().unwrap()
}

fn said(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn a_complete_release_passes() {
    let out = Release::new().check();
    assert!(out.status.success(), "{}", said(&out));
}

#[test]
fn a_platform_that_didnt_build_stops_the_release() {
    let release = Release::new();
    std::fs::remove_file(release.dist().join("tr3x-linux-aarch64.tar.gz")).unwrap();
    std::fs::remove_file(release.dist().join("tr3x-linux-aarch64.tar.gz.sha256")).unwrap();
    let out = release.check();
    assert!(!out.status.success());
    assert!(said(&out).contains("tr3x-linux-aarch64.tar.gz is missing"));
}

#[test]
fn an_archive_without_its_checksum_stops_the_release() {
    let release = Release::new();
    std::fs::remove_file(release.dist().join("tr3x-macos-universal.tar.gz.sha256")).unwrap();
    let out = release.check();
    assert!(!out.status.success());
    assert!(said(&out).contains("tr3x-macos-universal.tar.gz.sha256 is missing"));
}

#[test]
fn an_archive_that_changed_after_its_checksum_stops_the_release() {
    let release = Release::new();
    let sums = release.dist().join("tr3x-linux-x86_64.tar.gz.sha256");
    let text = std::fs::read_to_string(&sums).unwrap();
    // A different hash from the archive's, whichever digit it began with.
    let flipped = if text.starts_with('0') { "1" } else { "0" };
    std::fs::write(&sums, format!("{flipped}{}", &text[1..])).unwrap();
    let out = release.check();
    assert!(!out.status.success());
    assert!(said(&out).contains("tr3x-linux-x86_64.tar.gz doesn't match its checksum"));
}

#[test]
fn a_checksum_for_another_file_stops_the_release() {
    let release = Release::new();
    let sums = release.dist().join("tr3x-linux-x86_64.tar.gz.sha256");
    let text = std::fs::read_to_string(&sums).unwrap();
    std::fs::write(&sums, text.replace("tr3x-linux-x86_64", "tr3x-other")).unwrap();
    let out = release.check();
    assert!(!out.status.success());
    assert!(said(&out).contains("doesn't name tr3x-linux-x86_64.tar.gz"));
}

#[test]
fn an_archive_without_the_upstream_license_stops_the_release() {
    let release = Release::new();
    let without: Vec<&str> = FILES
        .into_iter()
        .filter(|f| *f != "LICENSE-t3client")
        .collect();
    release.pack("tr3x-linux-x86_64", &without, "");
    let out = release.check();
    assert!(!out.status.success());
    assert!(said(&out).contains("tr3x-linux-x86_64.tar.gz has no LICENSE-t3client"));
}

#[test]
fn the_upstream_license_has_to_keep_its_copyright_notice() {
    let release = Release::new();
    release.pack("tr3x-macos-universal", &FILES, "MIT License");
    let out = release.check();
    assert!(!out.status.success());
    assert!(said(&out).contains("no upstream copyright notice"));
}

#[test]
fn every_problem_is_listed_not_only_the_first() {
    let release = Release::new();
    std::fs::remove_file(release.dist().join("tr3x-linux-x86_64.tar.gz")).unwrap();
    release.pack("tr3x-macos-universal", &["tr3x"], "");
    let out = release.check();
    let said = said(&out);
    assert!(
        said.contains("tr3x-linux-x86_64.tar.gz is missing"),
        "{said}"
    );
    assert!(
        said.contains("tr3x-macos-universal.tar.gz has no THIRD-PARTY-LICENSES.txt"),
        "{said}"
    );
}
