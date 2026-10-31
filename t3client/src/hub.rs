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
        self.envs
            .iter()
            .filter(|e| self.store.credential(e.id()).is_some())
            .count()
    }

    /// Every machine's shell subscription in one stream, each event with its machine's id.
    /// Each machine reconnects on its own; one that's out of reach holds up none of the rest.
    pub fn watch_shells(&self) -> mpsc::UnboundedReceiver<(String, WatchEvent)> {
        let (tx, rx) = mpsc::unbounded_channel();
        for env in &self.envs {
            Hub::forward(env.clone(), tx.clone());
        }
        rx
    }

    /// Adds `env`'s shell subscription to a stream `watch_shells` made.
    pub fn forward(env: Arc<Env>, tx: mpsc::UnboundedSender<(String, WatchEvent)>) {
        let mut events = env.watch_shell(None);
        let id = env.id().to_string();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    event = events.recv() => match event {
                        Some(event) => if tx.send((id.clone(), event)).is_err() { return },
                        None => return,
                    },
                    _ = tx.closed() => return,
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
