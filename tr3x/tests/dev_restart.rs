//! `scripts/dev-restart` against a fixture of fake commands: nothing real is built, killed or
//! started, and nothing near an installed setup is touched.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

fn script(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A checkout holding the script under test, and fakes for what it runs. `cargo` builds
/// (or fails, with `cargo_exit`), `pkill` and the `rex` kill record themselves in `calls`.
struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new(cargo_exit: u8) -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        let real = Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/dev-restart");
        let copy = dir.join("checkout/scripts/dev-restart");
        script(
            &copy,
            &std::fs::read_to_string(real)
                .unwrap()
                .replacen("#!/bin/sh\n", "", 1),
        );
        let calls = dir.join("calls");
        script(
            &dir.join("bin/cargo"),
            &format!(
                "echo \"cargo $*\" >> '{calls}'\n\
                 [ {cargo_exit} -eq 0 ] || echo 'error[E0425]: cannot find value `x`' >&2\n\
                 exit {cargo_exit}",
                calls = calls.display()
            ),
        );
        script(
            &dir.join("bin/pkill"),
            &format!("echo \"pkill $*\" >> '{}'", calls.display()),
        );
        script(
            &dir.join("rex"),
            &format!("echo \"rex $*\" >> '{}'", calls.display()),
        );
        script(
            &dir.join("checkout/target/release/tr3x"),
            &format!(
                "echo started >> '{}'; echo synced > \"$TR3X_STATE_DIR/sync.log\"",
                calls.display()
            ),
        );
        std::fs::create_dir_all(dir.join("state")).unwrap();
        std::fs::write(
            dir.join("state/state.json"),
            r#"{"version":2,"groups":{"":{"sessionId":"s1"}}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("state/sync.log"), "old log").unwrap();
        Fixture { root }
    }

    fn run(&self) -> Output {
        let dir = self.root.path();
        let path = format!(
            "{}:{}",
            dir.join("bin").display(),
            std::env::var("PATH").unwrap()
        );
        Command::new("sh")
            .arg(dir.join("checkout/scripts/dev-restart"))
            .env("PATH", path)
            .env("TR3X_STATE_DIR", dir.join("state"))
            .env("TR3X_REX", dir.join("rex"))
            .env("WAIT", "1")
            .output()
            .unwrap()
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.root.path().join("calls")).unwrap_or_default()
    }
}

#[test]
fn a_failed_build_restarts_nothing_and_says_why() {
    let fixture = Fixture::new(1);
    let out = fixture.run();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("error[E0425]"),
        "{out:?}"
    );
    // Only the build ran: no kill, no deleted state, no new sync.
    assert_eq!(fixture.calls().lines().count(), 1, "{}", fixture.calls());
    let state = fixture.root.path().join("state");
    assert!(state.join("state.json").exists());
    assert_eq!(
        std::fs::read_to_string(state.join("sync.log")).unwrap(),
        "old log"
    );
}

#[test]
fn a_good_build_closes_the_old_sessions_and_starts_sync_again() {
    let fixture = Fixture::new(0);
    let out = fixture.run();
    assert!(out.status.success(), "{out:?}");
    let calls = fixture.calls();
    let lines: Vec<&str> = calls.lines().collect();
    assert!(lines[0].starts_with("cargo build --release"), "{calls}");
    assert!(lines[1].starts_with("pkill"), "{calls}");
    assert!(calls.contains("rex --autostart=false kill s1"), "{calls}");
    assert!(calls.contains("started"), "{calls}");
    assert!(!fixture.root.path().join("state/state.json").exists());
}

#[test]
fn the_installed_state_is_refused_before_anything_runs() {
    let fixture = Fixture::new(0);
    let home = fixture.root.path().join("home");
    let installed = home.join(".local/state/tr3x");
    std::fs::create_dir_all(&installed).unwrap();
    let out = Command::new("sh")
        .arg(fixture.root.path().join("checkout/scripts/dev-restart"))
        .env("HOME", &home)
        .env("TR3X_STATE_DIR", &installed)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(fixture.calls(), "");
}
