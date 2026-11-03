//! What tr3x keeps about the machines it's paired with, in its state directory:
//!
//! - `envs.json`: each machine's id, T3's name for it, the alias the user gave it, where it
//!   answers and how it was paired. Nothing secret.
//! - `credentials.json`, mode 0600: each machine's bearer token, its scopes and when it
//!   expires. Every tr3x process reads it; pairing writes it under `pair.lock`.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// How a machine was paired, which is how it pairs again when its token runs out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Via {
    /// This machine's own T3, through its CLI.
    Local,
    /// Over SSH, through the T3 CLI on that machine.
    Ssh,
    /// A pairing link the user pasted. Pairing again takes another.
    Link,
    /// The machine joined with `curl …/join | sh`. Pairing again takes running it again.
    Join,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    pub id: String,
    /// T3's name for the machine, from its descriptor.
    #[serde(default)]
    pub label: String,
    /// What the user calls it in Rex, from `tr3x env rename`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// Where it answers, best first.
    #[serde(default)]
    pub origins: Vec<String>,
    /// The SSH host it was paired through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<String>,
    pub via: Via,
    #[serde(default)]
    pub version: String,
}

impl Machine {
    /// The name Rex shows: the alias, else T3's label, else the start of its id.
    pub fn name(&self) -> &str {
        self.alias
            .as_deref()
            .filter(|a| !a.is_empty())
            .or(Some(self.label.as_str()).filter(|l| !l.is_empty()))
            .unwrap_or(&self.id[..self.id.len().min(8)])
    }

    pub fn is_local(&self) -> bool {
        self.via == Via::Local
    }

    /// Whether `query` names this machine: its id or the start of it, its alias or label,
    /// ignoring case, or its SSH host.
    pub fn answers_to(&self, query: &str) -> bool {
        let q = query.trim();
        if q.is_empty() {
            return false;
        }
        (q.len() >= 4 && self.id.starts_with(q))
            || [
                Some(self.label.as_str()),
                self.alias.as_deref(),
                self.ssh.as_deref(),
            ]
            .into_iter()
            .flatten()
            .any(|name| name.eq_ignore_ascii_case(q))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Registry {
    #[serde(default)]
    pub version: u32,
    /// This machine's environment id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
    #[serde(default)]
    pub machines: Vec<Machine>,
}

impl Registry {
    pub fn get(&self, id: &str) -> Option<&Machine> {
        self.machines.iter().find(|m| m.id == id)
    }

    /// The machine `query` names, when exactly one does.
    pub fn find(&self, query: &str) -> Option<&Machine> {
        if let Some(m) = self.get(query) {
            return Some(m);
        }
        let mut found = self.machines.iter().filter(|m| m.answers_to(query));
        let first = found.next()?;
        found.next().is_none().then_some(first)
    }

    /// Adds `machine`, or updates the one with its id, keeping its alias unless it brings one.
    pub fn upsert(&mut self, mut machine: Machine) {
        match self.machines.iter_mut().find(|m| m.id == machine.id) {
            Some(known) => {
                if machine.alias.is_none() {
                    machine.alias = known.alias.take();
                }
                for origin in std::mem::take(&mut known.origins) {
                    if !machine.origins.contains(&origin) {
                        machine.origins.push(origin);
                    }
                }
                *known = machine;
            }
            None => self.machines.push(machine),
        }
    }
}

/// A machine's bearer token. Never printed.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Credential {
    pub token: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Milliseconds since the epoch.
    pub issued_at: i64,
    pub expires_at: i64,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("scopes", &self.scopes)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// Within a day of expiring, a token is replaced when the machine can pair again on its own.
pub const RENEW_WITHIN_MS: i64 = 24 * 60 * 60 * 1000;

impl Credential {
    pub fn expired(&self, now: i64) -> bool {
        self.expires_at <= now
    }

    pub fn due(&self, now: i64) -> bool {
        self.expires_at - now < RENEW_WITHIN_MS
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Credentials {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    machines: BTreeMap<String, Credential>,
}

#[cfg(test)]
thread_local! {
    /// How often this test's thread has read the credentials file.
    pub(crate) static CREDENTIAL_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// tr3x's machine files in one state directory.
#[derive(Debug, Clone)]
pub struct Store {
    pub dir: PathBuf,
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Store {
        Store { dir: dir.into() }
    }

    fn envs_path(&self) -> PathBuf {
        self.dir.join("envs.json")
    }

    pub fn credentials_path(&self) -> PathBuf {
        self.dir.join("credentials.json")
    }

    pub fn registry(&self) -> Registry {
        std::fs::read(self.envs_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save_registry(&self, registry: &Registry) -> Result<()> {
        let mut registry = registry.clone();
        registry.version = 1;
        write_atomic(
            &self.envs_path(),
            &serde_json::to_vec_pretty(&registry)?,
            0o644,
        )
    }

    /// Changes the registry as it is on disk now, under the pairing lock.
    pub async fn update_registry(&self, change: impl FnOnce(&mut Registry)) -> Result<Registry> {
        let _lock = self.lock().await?;
        let mut registry = self.registry();
        change(&mut registry);
        self.save_registry(&registry)?;
        Ok(registry)
    }

    fn credentials(&self) -> Credentials {
        #[cfg(test)]
        CREDENTIAL_READS.with(|n| n.set(n.get() + 1));
        std::fs::read(self.credentials_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn credential(&self, id: &str) -> Option<Credential> {
        self.credentials().machines.remove(id)
    }

    /// How many of the machines `ids` hold a token, from one read of the credentials file.
    /// Tokens for machines not in `ids` don't count.
    pub fn count_credentials<'a>(&self, ids: impl IntoIterator<Item = &'a str>) -> usize {
        let all = self.credentials();
        ids.into_iter()
            .filter(|id| all.machines.contains_key(*id))
            .count()
    }

    /// Saves `credential` for machine `id`. Callers hold the pairing lock.
    pub fn save_credential(&self, id: &str, credential: Credential) -> Result<()> {
        let mut all = self.credentials();
        all.version = 1;
        all.machines.insert(id.to_string(), credential);
        write_atomic(
            &self.credentials_path(),
            &serde_json::to_vec_pretty(&all)?,
            0o600,
        )
    }

    /// Forgets machine `id`'s token. Callers hold the pairing lock.
    pub fn remove_credential(&self, id: &str) -> Result<bool> {
        let mut all = self.credentials();
        let removed = all.machines.remove(id).is_some();
        if removed {
            write_atomic(
                &self.credentials_path(),
                &serde_json::to_vec_pretty(&all)?,
                0o600,
            )?;
        }
        Ok(removed)
    }

    /// The lock every tr3x process holds while it pairs or changes these files, so two
    /// panes that find a token rejected at once pair only once. Waits up to a minute.
    pub async fn lock(&self) -> Result<std::fs::File> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.dir.join("pair.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("couldn't open {}", path.display()))?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(file),
                Err(std::fs::TryLockError::WouldBlock)
                    if tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(e) => anyhow::bail!("couldn't lock {}: {e}", path.display()),
            }
        }
    }
}

/// Writes through a temporary file with `mode`, so a reader never sees half a file and a
/// token is never readable by others, even for a moment.
fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(mode)
        .open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn machine(id: &str, label: &str) -> Machine {
        Machine {
            id: id.into(),
            label: label.into(),
            alias: None,
            origins: vec![format!("http://{label}:3773")],
            ssh: None,
            via: Via::Link,
            version: String::new(),
        }
    }

    #[test]
    fn machines_answer_to_their_names() {
        let mut registry = Registry::default();
        registry.upsert(machine("ddf826b2-3408", "mac"));
        registry.upsert(machine("1ec9e653-d01c", "Laptop (2)"));
        assert_eq!(registry.find("MAC").unwrap().id, "ddf826b2-3408");
        assert_eq!(registry.find("1ec9").unwrap().label, "Laptop (2)");
        assert!(registry.find("air").is_none());
        let mut renamed = machine("ddf826b2-3408", "mac");
        renamed.alias = Some("studio".into());
        registry.upsert(renamed);
        assert_eq!(registry.find("studio").unwrap().name(), "studio");
        // Pairing again keeps the alias and adds the new address after the old.
        let mut again = machine("ddf826b2-3408", "mac");
        again.origins = vec!["http://100.64.0.7:3773".into()];
        registry.upsert(again);
        let m = registry.get("ddf826b2-3408").unwrap();
        assert_eq!(m.alias.as_deref(), Some("studio"));
        assert_eq!(m.origins.len(), 2);
    }

    #[tokio::test]
    async fn credentials_are_private_and_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let _lock = store.lock().await.unwrap();
        let credential = Credential {
            token: "secret".into(),
            scopes: vec!["orchestration:read".into()],
            issued_at: 1,
            expires_at: 2,
        };
        store.save_credential("env-a", credential.clone()).unwrap();
        let mode = std::fs::metadata(store.credentials_path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(store.credential("env-a"), Some(credential.clone()));
        assert!(!format!("{credential:?}").contains("secret"));
        assert!(store.remove_credential("env-a").unwrap());
        assert_eq!(store.credential("env-a"), None);
    }

    #[tokio::test]
    async fn paired_machines_are_counted_from_one_read() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let ids = ["a", "b", "c"];
        // No file, then a file that isn't one: nobody is paired, and nothing breaks.
        assert_eq!(store.count_credentials(ids), 0);
        std::fs::write(store.credentials_path(), b"{not json").unwrap();
        assert_eq!(store.count_credentials(ids), 0);
        assert_eq!(store.count_credentials([]), 0);

        let _lock = store.lock().await.unwrap();
        let credential = |token: &str| Credential {
            token: token.into(),
            scopes: Vec::new(),
            issued_at: 1,
            expires_at: 2,
        };
        for id in ["a", "c", "not-in-the-hub"] {
            store.save_credential(id, credential(id)).unwrap();
        }
        CREDENTIAL_READS.with(|n| n.set(0));
        // A token for a machine that isn't asked about doesn't count.
        assert_eq!(store.count_credentials(ids), 2);
        assert_eq!(CREDENTIAL_READS.with(std::cell::Cell::get), 1);
        // Not remembered: the next count sees a machine forgotten, and one paired.
        store.remove_credential("a").unwrap();
        store.save_credential("b", credential("b")).unwrap();
        assert_eq!(store.count_credentials(ids), 2);
        assert_eq!(store.count_credentials(["a"]), 0);
    }
}
