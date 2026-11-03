//! Every machine t3rex is paired with, each with a connection of its own, and the thread
//! references that name a thread on one of them.

use std::fmt;
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::descriptor::Local;
use crate::env::{Env, WatchEvent};
use crate::store::{Machine, Registry, Store, Via};

/// A thread on one machine, written `env/thread`. T3's thread ids never hold a `/`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ThreadRef {
    pub env: String,
    pub thread: String,
}

impl ThreadRef {
    pub fn new(env: impl Into<String>, thread: impl Into<String>) -> ThreadRef {
        ThreadRef {
            env: env.into(),
            thread: thread.into(),
        }
    }

    /// `env/thread`, or a bare thread id, which is on `home`: what panes started before
    /// t3rex knew more than one machine pass.
    pub fn parse(text: &str, home: &str) -> ThreadRef {
        match text.split_once('/') {
            Some((env, thread)) if !env.is_empty() => ThreadRef::new(env, thread),
            _ => ThreadRef::new(home, text),
        }
    }
}

impl fmt::Display for ThreadRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.env, self.thread)
    }
}

pub struct Hub {
    pub store: Store,
    pub registry: Registry,
    envs: Vec<Arc<Env>>,
}

impl Hub {
    /// The machines in `dir`'s registry, with this machine's T3 among them even before it's
    /// paired: its connection pairs it through its own CLI the first time. Reads files only.
    pub fn load(dir: impl Into<std::path::PathBuf>) -> Hub {
        let store = Store::new(dir);
        let mut registry = store.registry();
        let local = Local::read();
        let home = registry.home.clone().or(local.environment_id.clone());
        if let Some(id) = &home
            && registry.get(id).is_none()
        {
            registry.machines.insert(
                0,
                Machine {
                    id: id.clone(),
                    label: String::new(),
                    alias: None,
                    origins: Vec::new(),
                    ssh: None,
                    via: Via::Local,
                    version: String::new(),
                },
            );
        }
        registry.home = home;
        let envs = registry
            .machines
            .iter()
            .map(|m| Arc::new(Env::new(store.clone(), m.clone())))
            .collect();
        Hub {
            store,
            registry,
            envs,
        }
    }

    pub fn envs(&self) -> &[Arc<Env>] {
        &self.envs
    }

    pub fn home_id(&self) -> Option<&str> {
        self.registry.home.as_deref()
    }

    pub fn home(&self) -> Option<Arc<Env>> {
        self.get(self.home_id()?)
    }

    /// The machine `query` names: its id or the start of one, its alias, label or SSH host.
    pub fn get(&self, query: &str) -> Option<Arc<Env>> {
        let id = self.registry.find(query)?.id.clone();
        self.envs.iter().find(|e| e.id() == id).cloned()
    }

    /// The machine with an id that starts with `prefix`, as window tags shorten them.
    pub fn by_prefix(&self, prefix: &str) -> Option<Arc<Env>> {
        let mut found = self.envs.iter().filter(|e| e.id().starts_with(prefix));
        let first = found.next()?;
        found.next().is_none().then(|| first.clone())
    }

    /// How many machines t3rex holds a token for.
    pub fn paired(&self) -> usize {
        self.store
            .count_credentials(self.envs.iter().map(|e| e.id()))
    }

    /// Every machine's shell subscription in one stream, each event with its machine's id.
    /// Each machine reconnects on its own; one that's out of reach holds up none of the rest.
    /// The subscriptions last until the stream is dropped.
    pub fn watch_shells(&self) -> mpsc::Receiver<(String, WatchEvent)> {
        let (tx, rx) = mpsc::channel(32);
        for env in &self.envs {
            spawn_forward(env.clone(), tx.clone());
        }
        rx
    }

    /// Adds `env`'s shell subscription to a stream. It lasts as long as the `Forwarder`
    /// returned, which the caller keeps for as long as it follows the machine.
    pub fn forward(env: Arc<Env>, tx: mpsc::Sender<(String, WatchEvent)>) -> Forwarder {
        Forwarder(spawn_forward(env, tx))
    }
}

/// One machine's shell subscription feeding a stream. Dropping it ends the subscription, and
/// with it the reconnecting: a machine that's forgotten, or a stream nobody reads any more,
/// isn't dialled for good. Replacing it with a new `forward` leaves one subscription, not two.
pub struct Forwarder(tokio::task::JoinHandle<()>);

impl Drop for Forwarder {
    fn drop(&mut self) {
        // The task owns the watch's receiver; dropping that tells the watch to stop.
        self.0.abort();
    }
}

fn spawn_forward(
    env: Arc<Env>,
    tx: mpsc::Sender<(String, WatchEvent)>,
) -> tokio::task::JoinHandle<()> {
    let mut events = env.watch_shell(None);
    let id = env.id().to_string();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                event = events.recv() => match event {
                    Some(event) => if tx.send((id.clone(), event)).await.is_err() { return },
                    None => return,
                },
                _ = tx.closed() => return,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn paired_reads_the_credentials_once_however_many_machines() {
        use crate::store::{CREDENTIAL_READS, Credential};

        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let machine = |id: &str| Machine {
            id: id.into(),
            label: id.into(),
            alias: None,
            origins: vec![format!("http://{id}:3773")],
            ssh: None,
            via: Via::Link,
            version: String::new(),
        };
        let mut registry = Registry::default();
        for id in ["a", "b", "c", "d"] {
            registry.upsert(machine(id));
        }
        store.save_registry(&registry).unwrap();
        let _lock = store.lock().await.unwrap();
        for id in ["a", "c", "orphan"] {
            let credential = Credential {
                token: id.into(),
                scopes: Vec::new(),
                issued_at: 1,
                expires_at: 2,
            };
            store.save_credential(id, credential).unwrap();
        }
        let hub = Hub::load(dir.path());
        CREDENTIAL_READS.with(|n| n.set(0));
        assert_eq!(hub.paired(), 2);
        assert_eq!(CREDENTIAL_READS.with(std::cell::Cell::get), 1);
    }

    /// A forwarded subscription ends when its `Forwarder` is dropped: the fake T3 is told the
    /// stream is interrupted, and nothing dials it again. Held, it stays subscribed.
    #[tokio::test]
    async fn dropping_a_forwarder_ends_its_machines_subscription() {
        use std::time::Duration;

        use serde_json::json;
        use tokio::net::TcpListener;

        use crate::env::tests::{client_at, open, recv};

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (tx, mut events) = mpsc::channel(32);
        let forwarder = Hub::forward(client_at(&origin), tx);

        let mut tickets = 0;
        let (_, mut socket) = open(&listener, &mut tickets).await;
        let request = loop {
            let frame = recv(&mut socket).await;
            if frame["tag"] == "orchestration.subscribeShell" {
                break frame;
            }
        };
        // Held, the subscription is quiet: no interrupt arrives.
        let quiet = tokio::time::timeout(Duration::from_millis(300), recv(&mut socket)).await;
        assert!(quiet.is_err(), "interrupted while held: {quiet:?}");

        drop(forwarder);
        let interrupt =
            json!({"_tag": "Interrupt", "requestId": request["id"], "interruptors": []});
        assert_eq!(recv(&mut socket).await, interrupt);
        // And it stays ended: nothing reconnects, and the stream hears no more from the machine.
        drop(socket);
        let again = tokio::time::timeout(Duration::from_millis(900), open(&listener, &mut tickets));
        assert!(again.await.is_err(), "dialled again after the drop");
        while let Ok(event) = events.try_recv() {
            assert!(
                matches!(
                    event,
                    (_, WatchEvent::Item(_) | WatchEvent::Reconnecting { .. })
                ),
                "{event:?}"
            );
        }
    }

    #[test]
    fn thread_refs_round_trip_and_bare_ids_are_home() {
        let r = ThreadRef::parse("env-a/thread:delegated-task:x%3Ay", "home");
        assert_eq!(r, ThreadRef::new("env-a", "thread:delegated-task:x%3Ay"));
        assert_eq!(r.to_string(), "env-a/thread:delegated-task:x%3Ay");
        assert_eq!(
            ThreadRef::parse("t-1", "home"),
            ThreadRef::new("home", "t-1")
        );
    }
}
