//! Keeping tr3x, and T3 on every machine Rex follows, up to date without a terminal.
//!
//! Sync checks once a day. tr3x's newest release comes from GitHub; with the `updates` setting
//! on `auto` (the default) sync installs it in the background, the way `install.sh` does: a
//! download checked against its SHA-256, then the new binary's own `install --update`, which
//! swaps the binary, rewrites Rex's config and starts sync again on it. Each pane starts itself
//! again on the new binary once nothing is typed in it (`icons::replaced`).
//!
//! T3 is never updated on its own, since an update restarts what runs the agents. Sync asks each
//! machine's T3 for the newest release on its track (`server.getNpmRelease`), and Reply says
//! when one is out. **Updates…** in the palette, `/update` or `tr3x update` installs it through
//! T3 itself (`server.updateServer`): the desktop app downloads its update and installs it when
//! it restarts (`server.commitDesktopUpdate`); a machine running T3's background service hands
//! it to its launcher. A plain `t3 serve` can't be updated from outside.
//!
//! What the checks found is in `updates.json` in tr3x's state directory, for Reply to read.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::t3;

/// The release asset `install.sh` and the updater download.
fn asset_name() -> String {
    format!("{}.tar.gz", crate::platform::release_name())
}
const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// The scope T3 asks for before it updates itself.
const MAINTAIN: &str = "environment:maintain";
/// The column machine names take in `tr3x update`.
const NAME: usize = 16;

/// How tr3x itself is kept up to date: the `updates` setting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    /// Install a new release in the background. The default.
    Auto,
    /// Say a release is out, and leave installing it to you.
    Notify,
    /// Never ask GitHub.
    Off,
}

pub fn mode() -> Mode {
    match crate::width::settings()
        .and_then(|s| s.get("updates").and_then(Value::as_str).map(str::to_string))
        .as_deref()
    {
        Some("notify") => Mode::Notify,
        Some("off") => Mode::Off,
        _ => Mode::Auto,
    }
}

/// What `tr3x updates` takes: `auto`, `notify` or `off`, or nothing to say which it is.
pub fn apply_mode(arg: &str) -> Result<String> {
    let name = |m: Mode| match m {
        Mode::Auto => "auto: tr3x installs its updates itself",
        Mode::Notify => "notify: Reply says when tr3x has an update",
        Mode::Off => "off: tr3x never checks for its updates",
    };
    let mode = match arg.trim() {
        "" => return Ok(format!("Updates: {}", name(mode()))),
        "auto" | "on" => Mode::Auto,
        "notify" | "ask" => Mode::Notify,
        "off" | "never" => Mode::Off,
        other => bail!("Can't read {other:?}: auto, notify or off"),
    };
    crate::width::set_setting(
        "updates",
        match mode {
            Mode::Auto => None,
            Mode::Notify => Some(json!("notify")),
            Mode::Off => Some(json!("off")),
        },
    )?;
    Ok(format!("Updates: {}", name(mode)))
}

/// Whether version `a` comes after `b`, in semver's order: numbers by value, a prerelease
/// before its release, and prerelease parts by number where both are numbers.
pub fn newer(a: &str, b: &str) -> bool {
    order(a, b) == std::cmp::Ordering::Greater
}

fn order(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let split = |v: &str| {
        let v = v.trim().trim_start_matches('v');
        let v = v.split('+').next().unwrap_or(v);
        match v.split_once('-') {
            Some((core, pre)) => (core.to_string(), Some(pre.to_string())),
            None => (v.to_string(), None),
        }
    };
    let ((core_a, pre_a), (core_b, pre_b)) = (split(a), split(b));
    let numbers =
        |core: &str| -> Vec<u64> { core.split('.').map(|n| n.parse().unwrap_or(0)).collect() };
    let (na, nb) = (numbers(&core_a), numbers(&core_b));
    for i in 0..na.len().max(nb.len()) {
        let c = na.get(i).unwrap_or(&0).cmp(nb.get(i).unwrap_or(&0));
        if c != Ordering::Equal {
            return c;
        }
    }
    match (pre_a, pre_b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(pa), Some(pb)) => {
            let (ia, ib): (Vec<&str>, Vec<&str>) =
                (pa.split('.').collect(), pb.split('.').collect());
            for i in 0..ia.len().max(ib.len()) {
                let c = match (ia.get(i), ib.get(i)) {
                    (None, None) => Ordering::Equal,
                    (None, Some(_)) => Ordering::Less,
                    (Some(_), None) => Ordering::Greater,
                    (Some(x), Some(y)) => match (x.parse::<u64>(), y.parse::<u64>()) {
                        (Ok(x), Ok(y)) => x.cmp(&y),
                        (Ok(_), Err(_)) => Ordering::Less,
                        (Err(_), Ok(_)) => Ordering::Greater,
                        (Err(_), Err(_)) => x.cmp(y),
                    },
                };
                if c != Ordering::Equal {
                    return c;
                }
            }
            Ordering::Equal
        }
    }
}

/// The npm dist-tag a T3 version's track publishes under: a nightly stays on nightlies.
pub fn track(version: &str) -> &'static str {
    if version.contains("-nightly.") {
        "nightly"
    } else if version.contains("-preview.") {
        "preview"
    } else {
        "latest"
    }
}

/// What the last check found.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notes {
    /// When it last looked, in milliseconds since the epoch.
    #[serde(default)]
    pub checked_at: i64,
    /// tr3x's newest release, when GitHub answered.
    #[serde(default)]
    pub tr3x: Option<String>,
    /// Each machine's T3, by environment id.
    #[serde(default)]
    pub machines: BTreeMap<String, Machine>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    pub name: String,
    pub version: String,
    pub latest: String,
    /// How T3 runs there, as its `serverSelfUpdate` capability says: `desktop-managed`,
    /// `boot-service` or `respawn`. None: it can't be updated from outside.
    #[serde(default)]
    pub how: Option<String>,
}

impl Machine {
    pub fn behind(&self) -> bool {
        !self.latest.is_empty() && newer(&self.latest, &self.version)
    }
}

impl Notes {
    /// tr3x's newer release, if there is one.
    pub fn tr3x_update(&self) -> Option<&str> {
        self.tr3x
            .as_deref()
            .filter(|v| newer(v, env!("CARGO_PKG_VERSION")))
    }

    /// The machine's T3 update, when it has one tr3x can install.
    pub fn t3_update(&self, env: &str) -> Option<&Machine> {
        self.machines
            .get(env)
            .filter(|m| m.behind() && m.how.is_some())
    }
}

fn notes_path() -> PathBuf {
    crate::state::dir().join("updates.json")
}

pub fn notes() -> Notes {
    std::fs::read(notes_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save(notes: &Notes) -> Result<()> {
    let path = notes_path();
    let tmp = path.with_extension("json.new");
    std::fs::write(&tmp, serde_json::to_vec_pretty(notes)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

/// What Reply shows beside its chip for a thread on `env`, as of the last check: T3's update
/// there, else tr3x's when it waits for you. Read again only when the file changes.
pub fn notice(env: &str) -> Option<String> {
    use std::sync::Mutex;
    use std::time::SystemTime;
    static CACHE: Mutex<Option<(Option<SystemTime>, Notes)>> = Mutex::new(None);
    let stamp = std::fs::metadata(notes_path())
        .and_then(|m| m.modified())
        .ok();
    let mut cache = CACHE.lock().ok()?;
    if cache.as_ref().is_none_or(|(at, _)| *at != stamp) {
        *cache = Some((stamp, notes()));
    }
    let notes = &cache.as_ref()?.1;
    if notes.t3_update(env).is_some() {
        return Some("T3 update · /update".into());
    }
    let waiting = mode() != Mode::Auto || !self_updates();
    notes
        .tr3x_update()
        .filter(|_| waiting)
        .map(|v| format!("tr3x {v} · /update"))
}

/// Whether this tr3x can replace itself: it runs from where `install` puts it. A build or a
/// copy anywhere else is updated the way it was made.
pub fn self_updates() -> bool {
    crate::icons::real_exe() == crate::install::bin_path()
}

// ---- GitHub ----------------------------------------------------------------------------------

struct Release {
    version: String,
    tarball: String,
    sum: String,
}

/// Where tr3x's latest release is described: GitHub's API for the repository this was built
/// from, or `TR3X_RELEASES` (a URL curl reads, `file://` too) to try another.
fn releases_url() -> String {
    std::env::var("TR3X_RELEASES").unwrap_or_else(|_| {
        let repo = env!("CARGO_PKG_REPOSITORY").trim_end_matches('/');
        let path = repo.trim_start_matches("https://github.com/");
        format!("https://api.github.com/repos/{path}/releases/latest")
    })
}

pub(crate) async fn curl(args: &[&str]) -> Result<Vec<u8>> {
    let out = tokio::process::Command::new("/usr/bin/curl")
        .args(["-fsSL", "--max-time", "120"])
        .args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .context("couldn't run curl")?;
    if !out.status.success() {
        bail!(
            "{}",
            String::from_utf8_lossy(&out.stderr)
                .trim()
                .trim_start_matches("curl: ")
        );
    }
    Ok(out.stdout)
}

async fn latest_release() -> Result<Release> {
    release_at(&releases_url()).await
}

async fn release_at(url: &str) -> Result<Release> {
    let body = curl(&["-H", "Accept: application/vnd.github+json", url])
        .await
        .context("couldn't ask GitHub for tr3x's latest release")?;
    let release: Value = serde_json::from_slice(&body).context("GitHub's answer wasn't JSON")?;
    let version = release["tag_name"]
        .as_str()
        .context("the release has no tag")?
        .trim_start_matches('v')
        .to_string();
    let name = asset_name();
    let asset = |name: &str| {
        release["assets"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|a| a["name"] == name)
            .and_then(|a| a["browser_download_url"].as_str())
            .map(str::to_string)
    };
    Ok(Release {
        tarball: asset(&name).with_context(|| format!("release {version} has no {name}"))?,
        sum: asset(&format!("{name}.sha256"))
            .with_context(|| format!("release {version} has no checksum"))?,
        version,
    })
}

/// Downloads the release, checks it against its SHA-256 and that it runs as that version, and
/// returns the new binary, in a folder of tr3x's state directory.
async fn fetch(release: &Release) -> Result<PathBuf> {
    fetch_into(release, &crate::state::dir().join("update")).await
}

async fn fetch_into(release: &Release, dir: &Path) -> Result<PathBuf> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir.join("x"))?;
    let tarball = dir.join(asset_name());
    let tarball_s = tarball.to_string_lossy().into_owned();
    curl(&["-o", &tarball_s, &release.tarball])
        .await
        .context("couldn't download the release")?;
    let sum = String::from_utf8(curl(&[&release.sum]).await?)?;
    let want = sum
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase();
    let got = sha256(&tarball).await?;
    if want.len() != 64 || got != want {
        bail!("the download's SHA-256 doesn't match the release's");
    }
    let x = dir.join("x");
    let status = tokio::process::Command::new("tar")
        .arg("-xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(&x)
        .status()
        .await?;
    if !status.success() {
        bail!("couldn't unpack the release");
    }
    let bin = find_binary(&x).context("the release has no tr3x in it")?;
    let out = tokio::process::Command::new(&bin)
        .arg("--version")
        .output()
        .await
        .context("the new tr3x doesn't run")?;
    let says = String::from_utf8_lossy(&out.stdout);
    if says.split_whitespace().nth(1) != Some(release.version.as_str()) {
        bail!("the new binary says it's {}", says.trim());
    }
    Ok(bin)
}

pub(crate) async fn sha256(path: &Path) -> Result<String> {
    // macOS has `shasum`; most Linux machines have `sha256sum`, and some only `shasum`.
    let (program, args): (&str, &[&str]) = if crate::platform::MAC || !on_path("sha256sum") {
        ("shasum", &["-a", "256"])
    } else {
        ("sha256sum", &[])
    };
    let out = tokio::process::Command::new(program)
        .args(args)
        .arg(path)
        .output()
        .await
        .with_context(|| format!("couldn't run {program} to check the download"))?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase())
}

fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|d| d.join(name).is_file()))
}

fn find_binary(dir: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_binary(&path) {
                return Some(found);
            }
        } else if entry.file_name() == "tr3x"
            && entry
                .metadata()
                .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
        {
            return Some(path);
        }
    }
    None
}

/// Installs `release` over this tr3x: the new binary's own `install --update`.
async fn install(release: &Release) -> Result<()> {
    let bin = fetch(release).await?;
    let status = tokio::process::Command::new(&bin)
        .args(["install", "--update"])
        .stdin(std::process::Stdio::null())
        .status()
        .await
        .context("couldn't run the new tr3x")?;
    if !status.success() {
        bail!("the new tr3x couldn't install itself");
    }
    let _ = std::fs::remove_dir_all(crate::state::dir().join("update"));
    Ok(())
}

// ---- T3 --------------------------------------------------------------------------------------

/// The newest T3 on `env`'s track, and how T3 runs there. Connecting fills in the descriptor.
async fn check_machine(env: &t3client::env::Env) -> Result<Machine> {
    // Connect first, for the descriptor's version.
    env.rpc().await?;
    let found = env.descriptor().context("no descriptor")?;
    let version = found.server_version.clone();
    let answer = env
        .call("server.getNpmRelease", json!({"spec": track(&version)}))
        .await?;
    let how = env.capabilities()["serverSelfUpdate"]
        .as_str()
        .map(str::to_string);
    Ok(Machine {
        name: env.name().to_string(),
        latest: answer["version"].as_str().unwrap_or_default().to_string(),
        version,
        how,
    })
}

/// Looks for updates to tr3x (unless the setting is off) and to T3 on every machine that
/// answers, and saves what it found.
pub async fn check() -> Notes {
    let mut notes = notes();
    if mode() != Mode::Off {
        match latest_release().await {
            Ok(release) => notes.tr3x = Some(release.version),
            Err(e) => eprintln!("tr3x: update check: {e:#}"),
        }
    }
    let hub = t3::hub();
    for env in hub.envs() {
        match tokio::time::timeout(Duration::from_secs(15), check_machine(env)).await {
            Ok(Ok(machine)) => {
                notes.machines.insert(env.id().to_string(), machine);
            }
            Ok(Err(e)) => eprintln!("tr3x: update check on {}: {e:#}", env.name()),
            Err(_) => eprintln!("tr3x: update check on {}: no answer", env.name()),
        }
    }
    notes.checked_at = t3::now_ms();
    if let Err(e) = save(&notes) {
        eprintln!("tr3x: couldn't save the update check: {e:#}");
    }
    notes
}

/// Sync's half: a check soon after it starts when the last is a day old, then daily. On `auto`
/// a new tr3x installs itself, in a process of its own, since installing stops this sync.
pub async fn follow() {
    tokio::time::sleep(Duration::from_secs(90)).await;
    loop {
        if t3::now_ms() - notes().checked_at >= DAY_MS {
            let notes = check().await;
            if let Some(version) = notes.tr3x_update()
                && mode() == Mode::Auto
                && self_updates()
            {
                println!("tr3x: installing {version}");
                spawn_auto();
            }
        }
        tokio::time::sleep(Duration::from_secs(60 * 60)).await;
    }
}

/// `tr3x update --auto` in its own process group, writing where sync writes.
fn spawn_auto() {
    let exe = crate::icons::real_exe();
    let spawned = std::process::Command::new(&exe)
        .args(["update", "--auto"])
        .stdin(std::process::Stdio::null())
        .process_group(0)
        .spawn();
    if let Err(e) = spawned {
        eprintln!("tr3x: couldn't start the update: {e}");
    }
}

use std::os::unix::process::CommandExt;

// ---- tr3x update ----------------------------------------------------------------------------

/// `tr3x update`: what's out, then, unless `check_only`, installs it. `auto` is sync's own run:
/// tr3x alone, nothing asked. In a terminal, it asks before anything restarts.
pub async fn run(check_only: bool, auto: bool, wait: bool) -> Result<()> {
    let result = run_inner(check_only, auto).await;
    if wait {
        if let Err(e) = &result {
            println!("\r\n  ✗ {e:#}");
        }
        tokio::time::sleep(Duration::from_secs(if result.is_ok() { 3 } else { 8 })).await;
    }
    result
}

async fn run_inner(check_only: bool, auto: bool) -> Result<()> {
    if auto {
        let release = latest_release().await?;
        if !newer(&release.version, env!("CARGO_PKG_VERSION")) {
            return Ok(());
        }
        if !self_updates() {
            bail!("this tr3x isn't the installed one; not updating it");
        }
        install(&release).await?;
        println!("tr3x: updated to {}", release.version);
        return Ok(());
    }
    println!("Checking for updates…\n");
    let release = if mode() == Mode::Off && check_only {
        None
    } else {
        match latest_release().await {
            Ok(r) => Some(r),
            Err(e) => {
                let said = format!("{e:#}");
                if said.contains("404") {
                    println!("  · {:<w$} no release on GitHub yet", "tr3x", w = NAME);
                } else {
                    println!("  ✗ {:<w$} couldn't check: {said}", "tr3x", w = NAME);
                }
                None
            }
        }
    };
    let ours = env!("CARGO_PKG_VERSION");
    let tr3x_update = release.as_ref().filter(|r| newer(&r.version, ours));
    match (&release, tr3x_update) {
        (_, Some(r)) => println!("  ↑ {:<w$} {ours} → {}", "tr3x", r.version, w = NAME),
        (Some(_), None) => println!("  ✓ {:<w$} {ours}", "tr3x", w = NAME),
        (None, None) => {}
    }
    let hub = t3::hub();
    let mut notes = notes();
    let mut todo: Vec<(Arc<t3client::env::Env>, Machine)> = Vec::new();
    for env in hub.envs() {
        let name = env.name().to_string();
        match tokio::time::timeout(Duration::from_secs(15), check_machine(env)).await {
            Ok(Ok(m)) => {
                notes.machines.insert(env.id().to_string(), m.clone());
                if !m.behind() {
                    println!("  ✓ {name:<w$} T3 {}", m.version, w = NAME);
                } else if m.how.is_none() {
                    println!(
                        "  · {name:<w$} T3 {} → {}  update it there, or run `t3 service install` there once",
                        m.version,
                        m.latest,
                        w = NAME
                    );
                } else {
                    println!("  ↑ {name:<w$} T3 {} → {}", m.version, m.latest, w = NAME);
                    todo.push((env.clone(), m));
                }
            }
            Ok(Err(e)) => println!("  ✗ {name:<w$} {e:#}", w = NAME),
            Err(_) => println!("  ✗ {name:<w$} didn't answer", w = NAME),
        }
    }
    if let Some(r) = &release {
        notes.tr3x = Some(r.version.clone());
    }
    notes.checked_at = t3::now_ms();
    let _ = save(&notes);
    let tr3x_update = tr3x_update.filter(|_| {
        if self_updates() {
            true
        } else {
            println!(
                "\n  This tr3x runs from {}, not {}: update it the way you installed it.",
                crate::icons::real_exe().display(),
                crate::install::bin_path().display()
            );
            false
        }
    });
    if check_only {
        return Ok(());
    }
    if tr3x_update.is_none() && todo.is_empty() {
        let behind = notes.machines.values().any(Machine::behind);
        println!(
            "\n{}",
            if behind {
                "Nothing here that tr3x can install."
            } else {
                "Everything's up to date."
            }
        );
        return Ok(());
    }
    let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin());
    if interactive && !confirm("\nEnter updates · Esc leaves it").await? {
        return Ok(());
    }
    println!();
    for (env, m) in todo {
        match update_machine(&env, &m, interactive).await {
            Ok(said) => println!("  ✓ {}: {said}", m.name),
            Err(e) => println!("  ✗ {}: {e:#}", m.name),
        }
    }
    if let Some(release) = tr3x_update {
        println!("  … tr3x {}", release.version);
        install(release).await?;
        println!("  ✓ tr3x: {}", release.version);
    }
    Ok(())
}

/// Updates T3 on one machine, through T3. A desktop app restarts to install, so with threads
/// running there it asks first, in a terminal, and otherwise leaves the update downloaded.
async fn update_machine(
    env: &t3client::env::Env,
    m: &Machine,
    interactive: bool,
) -> Result<String> {
    if !env.holds(MAINTAIN) {
        env.pair_fresh().await.with_context(|| {
            format!(
                "Rex's pairing with {} predates updates; pair it again with `tr3x pair {}`",
                m.name, m.name
            )
        })?;
    }
    println!("  … {}: downloading T3 {}", m.name, m.latest);
    let result = env
        .rpc()
        .await?
        .call(
            "server.updateServer",
            json!({"targetVersion": m.latest}),
            Duration::from_secs(15 * 60),
        )
        .await
        .map_err(|e| anyhow::anyhow!("{}", reason(&e)))?;
    let Some(token) = result["desktopUpdateToken"].as_str() else {
        // A background service hands the new version to its launcher, which restarts T3.
        return Ok(format!("T3 {} installed; it restarts by itself", m.latest));
    };
    let running = running_threads(env).await;
    if running > 0 {
        let ask = format!(
            "{} has {running} thread{} running. T3 Code restarts to install, and picks {} up again after. Restart now? Enter restarts · Esc later",
            m.name,
            if running == 1 { "" } else { "s" },
            if running == 1 { "it" } else { "them" }
        );
        if !interactive || !confirm(&format!("  {ask}")).await? {
            return Ok("downloaded; installs next time T3 Code restarts".into());
        }
    }
    let target = result["targetVersion"].as_str().unwrap_or(&m.latest);
    commit(env, token, target).await?;
    Ok(format!("T3 Code restarted on {target}"))
}

/// What T3 said went wrong: a `ServerSelfUpdateError`'s reason, else the error as tr3x puts it.
fn reason(e: &t3client::rpc::RpcError) -> String {
    if let t3client::rpc::RpcError::Failed { error, .. } = e
        && let Some(reason) = error["reason"].as_str()
    {
        return reason.to_string();
    }
    e.to_string()
}

async fn running_threads(env: &t3client::env::Env) -> usize {
    env.shell()
        .await
        .map(|s| {
            s.threads
                .iter()
                .filter(|t| t["activeRunId"].is_string())
                .count()
        })
        .unwrap_or(0)
}

/// Asks the desktop app to install what it downloaded. It quits to do so, which ends the
/// connection: that, rather than an answer, is how it goes. Then waits for it to come back.
async fn commit(env: &t3client::env::Env, token: &str, target: &str) -> Result<()> {
    let call = env
        .rpc()
        .await?
        .call(
            "server.commitDesktopUpdate",
            json!({"requestId": token}),
            Duration::from_secs(150),
        )
        .await;
    // A failure T3 names is one; a dropped connection is the app quitting to install.
    if let Err(e @ t3client::rpc::RpcError::Failed { .. }) = call {
        bail!("{}", reason(&e));
    }
    println!("  … {}: restarting T3 Code", env.name());
    // The old connection can look open for a moment, so it's back once it says the new version.
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_secs(3)).await;
        if env.rpc().await.is_ok() && env.descriptor().is_some_and(|d| d.server_version == target) {
            return Ok(());
        }
    }
    bail!("T3 Code hasn't come back after three minutes")
}

/// `/update`: **Update tr3x and T3…** in a floating window over the thread `block` is in.
pub async fn open_layer(block: &str) -> Result<()> {
    let rex = crate::rex::Rex::find();
    let session = std::env::var("REX_SESSION").context("not in a Rex session")?;
    let view = rex.inspect(&session).await?;
    let window = view["windows"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|w| {
            w["layers"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|l| l["blocks"].as_array().into_iter().flatten())
                .any(|b| b["block_id"] == block)
        })
        .and_then(|w| w["window_id"].as_str())
        .context("that block isn't in this session")?
        .to_string();
    let exe = crate::icons::real_exe().to_string_lossy().into_owned();
    let data = json!({
        "argv": crate::rex::pane_argv(&exe, &["update", "--wait"]),
        "window": window,
    });
    let script = format!(
        r#"local d = rex.json.decode({})
local _, err = rex.session.new_layer{{ window_id = d.window, bounds = {{ 0.15, 0.2, 0.7, 0.5 }}, focus = true,
  layout = {{ block = {{ flavor = "com.superlogical.terminal.shell", label = "Updates",
    options = {{ shell = "none", login = false, command = d.argv }} }} }} }}
rex.session.disown_block{{}}
if err then error(err, 0) end
return {{}}"#,
        crate::rex::lua_str(&data.to_string())
    );
    rex.lua(&session, &script).await?;
    rex.show_on_phones(&session).await
}

/// Enter answers yes, Esc or q no.
async fn confirm(question: &str) -> Result<bool> {
    use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind};
    use futures_util::StreamExt;
    println!("{question}");
    crossterm::terminal::enable_raw_mode()?;
    let mut input = EventStream::new();
    let answer = loop {
        match input.next().await {
            Some(Ok(Event::Key(k))) if k.kind != KeyEventKind::Release => match k.code {
                KeyCode::Enter | KeyCode::Char('y') => break true,
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('q') => break false,
                _ => {}
            },
            Some(Ok(_)) => {}
            _ => break false,
        }
    };
    crossterm::terminal::disable_raw_mode()?;
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_order_as_semver() {
        assert!(newer("0.2.0", "0.1.9"));
        assert!(newer("v0.10.0", "0.9.0"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(newer("0.1.6", "0.1.6-nightly.20261030.3917"));
        assert!(newer(
            "0.1.7-nightly.20261103.4106",
            "0.1.7-nightly.20261102.4066"
        ));
        assert!(newer(
            "0.1.7-nightly.20261102.4066",
            "0.1.6-nightly.20261030.3917"
        ));
        assert!(!newer("0.1.6", "0.1.7-nightly.20261102.4066"));
        assert!(newer("1.0.0-rc.10", "1.0.0-rc.9"));
    }

    #[test]
    fn a_track_stays_on_itself() {
        assert_eq!(track("0.1.7-nightly.20261102.4066"), "nightly");
        assert_eq!(track("0.1.6-preview.20261030.3948"), "preview");
        assert_eq!(track("0.1.6"), "latest");
    }

    #[test]
    fn only_a_machine_tr3x_can_update_has_a_t3_update() {
        let mut notes = Notes::default();
        let machine = |how: Option<&str>, version: &str| Machine {
            name: "studio".into(),
            version: version.into(),
            latest: "0.1.7".into(),
            how: how.map(str::to_string),
        };
        notes
            .machines
            .insert("a".into(), machine(Some("boot-service"), "0.1.6"));
        notes.machines.insert("b".into(), machine(None, "0.1.6"));
        notes
            .machines
            .insert("c".into(), machine(Some("desktop-managed"), "0.1.7"));
        assert!(notes.t3_update("a").is_some());
        assert!(notes.t3_update("b").is_none());
        assert!(notes.t3_update("c").is_none());
        assert!(notes.t3_update("missing").is_none());
    }

    #[test]
    fn tr3x_update_is_only_a_newer_release() {
        let mut notes = Notes::default();
        assert_eq!(notes.tr3x_update(), None);
        notes.tr3x = Some(env!("CARGO_PKG_VERSION").into());
        assert_eq!(notes.tr3x_update(), None);
        notes.tr3x = Some("99.0.0".into());
        assert_eq!(notes.tr3x_update(), Some("99.0.0"));
    }

    /// A release as GitHub describes one, served from files: the tarball, its checksum and a
    /// tr3x that says its version.
    fn release_files(dir: &Path, says: &str, sum: Option<&str>) -> String {
        use std::os::unix::fs::PermissionsExt;
        let pkg = dir.join("pkg").join(crate::platform::release_name());
        std::fs::create_dir_all(&pkg).unwrap();
        let bin = pkg.join("tr3x");
        std::fs::write(&bin, format!("#!/bin/sh\necho 'tr3x {says}'\n")).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let asset = asset_name();
        let tarball = dir.join(&asset);
        let ok = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&tarball)
            .arg("-C")
            .arg(dir.join("pkg"))
            .arg(crate::platform::release_name())
            .status()
            .unwrap();
        assert!(ok.success());
        let out = std::process::Command::new(if crate::platform::MAC {
            "shasum"
        } else {
            "sha256sum"
        })
        .args(if crate::platform::MAC {
            &["-a", "256"][..]
        } else {
            &[][..]
        })
        .arg(&tarball)
        .output()
        .unwrap();
        let real = String::from_utf8_lossy(&out.stdout).to_string();
        std::fs::write(dir.join(format!("{asset}.sha256")), sum.unwrap_or(&real)).unwrap();
        let url = |name: &str| format!("file://{}", dir.join(name).display());
        let json = json!({
            "tag_name": "v9.9.9",
            "assets": [
                {"name": asset, "browser_download_url": url(&asset)},
                {"name": format!("{asset}.sha256"), "browser_download_url": url(&format!("{asset}.sha256"))},
            ],
        });
        std::fs::write(dir.join("release.json"), json.to_string()).unwrap();
        url("release.json")
    }

    #[tokio::test]
    async fn a_release_downloads_checks_and_unpacks() {
        let dir = tempfile::tempdir().unwrap();
        let url = release_files(dir.path(), "9.9.9", None);
        let release = release_at(&url).await.unwrap();
        assert_eq!(release.version, "9.9.9");
        let bin = fetch_into(&release, &dir.path().join("update"))
            .await
            .unwrap();
        assert!(bin.ends_with(format!("{}/tr3x", crate::platform::release_name())));
    }

    #[tokio::test]
    async fn a_download_that_doesnt_match_its_checksum_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let url = release_files(dir.path(), "9.9.9", Some(&"0".repeat(64)));
        let release = release_at(&url).await.unwrap();
        let err = fetch_into(&release, &dir.path().join("update"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("SHA-256"), "{err:#}");
    }

    #[tokio::test]
    async fn a_binary_that_says_another_version_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let url = release_files(dir.path(), "1.0.0", None);
        let release = release_at(&url).await.unwrap();
        let err = fetch_into(&release, &dir.path().join("update"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("says it's tr3x 1.0.0"), "{err:#}");
    }

    #[test]
    fn notes_survive_a_round_trip() {
        let mut notes = Notes {
            checked_at: 5,
            tr3x: Some("0.2.0".into()),
            ..Notes::default()
        };
        notes.machines.insert(
            "env".into(),
            Machine {
                name: "studio".into(),
                version: "0.1.6".into(),
                latest: "0.1.7".into(),
                how: Some("boot-service".into()),
            },
        );
        let text = serde_json::to_string(&notes).unwrap();
        assert!(text.contains("\"checkedAt\":5"));
        assert_eq!(serde_json::from_str::<Notes>(&text).unwrap(), notes);
    }
}
