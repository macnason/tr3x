//! t3rex: Rex as the interface to T3 Code. T3 keeps running the agents and owns every thread's
//! state; Rex shows each open thread as a window in one session.

mod chat;
mod compose;
mod install;
mod media;
mod render;
mod rex;
mod state;
mod sync;
mod t3;

use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use crate::rex::Rex;
use crate::state::State;
use crate::t3::Lifecycle;

#[derive(Parser)]
#[command(name = "t3rex", version, about = "Rex as the interface to T3 Code.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Keep the "T3 Code" Rex session in step with T3. Runs until stopped.
    Sync {
        /// Leave the session where Rex puts new sessions, instead of first.
        #[arg(long)]
        no_first: bool,
    },
    /// The conversation pane of a thread's window.
    Chat { thread: String },
    /// The reply pane of a thread's window.
    Compose { thread: String },
    /// Print a thread's transcript blocks, for working on the chat pane.
    #[command(hide = true)]
    Blocks { thread: String },
    /// Settle the thread. Its window closes.
    Settle(Target),
    /// Bring a settled thread back.
    Unsettle(Target),
    /// Snooze the thread, for 2h, 30m, 1d and so on.
    Snooze {
        #[arg(default_value = "1h")]
        duration: String,
        #[command(flatten)]
        target: Target,
    },
    /// End the thread's snooze.
    Wake(Target),
    /// Pin the thread, or unpin it with --off.
    Pin {
        #[arg(long)]
        off: bool,
        #[command(flatten)]
        target: Target,
    },
    /// Archive the thread.
    Archive(Target),
    /// Open the thread in T3 Code.
    Open(Target),
    /// Print where the thread that most needs you is, as Rex's session.select takes it.
    Inbox,
    /// Print the thread a Rex block belongs to.
    Which(Target),
    /// Set up t3rex: the Rex config, the launchd agent, and a first sync.
    Install,
    /// Undo `install`: stop the agent, remove the config line, close t3rex's session.
    Uninstall,
    /// Check that Rex and T3 are reachable.
    Doctor,
}

/// Which thread a command acts on. Without flags, the thread of the Rex block it runs in.
#[derive(clap::Args, Clone)]
struct Target {
    /// A T3 thread id.
    #[arg(long)]
    thread: Option<String>,
    /// A Rex block in a t3rex window, as Rex actions pass `ctx.block_id`.
    #[arg(long, env = "REX_BLOCK")]
    block: Option<String>,
}

impl Target {
    async fn resolve(&self) -> Result<String> {
        if let Some(thread) = &self.thread {
            return Ok(thread.clone());
        }
        let Some(block) = &self.block else {
            bail!("name a thread with --thread, or run this in a T3 window in Rex");
        };
        let state = State::load();
        let session = state
            .session_id
            .clone()
            .context("t3rex hasn't synced yet")?;
        let view = Rex::find().inspect(&session).await?;
        let window = view["windows"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|w| {
                w["layers"].as_array().into_iter().flatten().any(|layer| {
                    layer["blocks"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|b| b["block_id"] == block.as_str())
                })
            })
            .and_then(|w| w["window_id"].as_str())
            .context("that block isn't in a T3 window")?;
        state
            .thread_for_window(window)
            .map(str::to_string)
            .context("that window isn't one of t3rex's")
    }
}

fn main() {
    // A temporary login per process, revoked on exit, keeps the Keychain out of it: a launchd
    // agent or a Rex pane can't answer a Keychain prompt.
    if std::env::var_os("T3REX_SAVED_LOGIN").is_none() {
        // SAFETY: no other thread exists yet.
        unsafe { std::env::set_var("T3TERM_NO_SAVED_LOGIN", "1") };
    }
    let cli = Cli::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let code = runtime.block_on(async move {
        tokio::select! {
            result = run(cli) => match result {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("t3rex: {error:#}");
                    1
                }
            },
            _ = tokio::signal::ctrl_c() => 130,
            code = terminated() => code,
        }
    });
    runtime.shutdown_timeout(Duration::from_millis(200));
    t3term::auth::revoke_all_sessions();
    std::process::exit(code);
}

async fn terminated() -> i32 {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut hup), Ok(mut term)) = (
        signal(SignalKind::hangup()),
        signal(SignalKind::terminate()),
    ) else {
        return std::future::pending().await;
    };
    tokio::select! {
        _ = hup.recv() => 129,
        _ = term.recv() => 143,
    }
}

async fn lifecycle(target: &Target, action: Lifecycle) -> Result<()> {
    let thread = target.resolve().await?;
    let client = t3::connect(true).await?;
    let first = client.dispatch(t3::command(&thread, &action)).await;
    // T3 won't settle or archive a thread mid-run. Settling says the thread is finished, so
    // stop the run, wait for T3 to see it stop, and try again.
    let running = matches!(&first, Err(e) if e.to_string().contains("still running"));
    if !(running && matches!(action, Lifecycle::Settle | Lifecycle::Archive)) {
        return first.map(drop);
    }
    let run_id = |shell: &t3term::projection::ShellState| {
        t3term::tui::find_thread_in(shell, &thread)
            .and_then(|t| t["activeRunId"].as_str())
            .map(str::to_string)
    };
    if let Some(run) = run_id(&client.shell().await?) {
        client
            .dispatch(serde_json::json!({"type": "run.interrupt", "threadId": thread, "runId": run, "reason": "Settled from Rex"}))
            .await?;
    }
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if run_id(&client.shell().await?).is_none() {
            break;
        }
    }
    client
        .dispatch(t3::command(&thread, &action))
        .await
        .map(drop)
}

async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Sync { no_first } => sync::run(!no_first).await,
        Command::Chat { thread } => chat::run(thread).await,
        Command::Compose { thread } => compose::run(thread).await,
        Command::Blocks { thread } => chat::dump(thread).await,
        Command::Settle(t) => lifecycle(&t, Lifecycle::Settle).await,
        Command::Unsettle(t) => lifecycle(&t, Lifecycle::Unsettle).await,
        Command::Snooze { duration, target } => {
            let ms = t3::snooze_ms(&duration).with_context(|| {
                format!("can't read {duration:?} as a duration, such as 2h or tomorrow")
            })?;
            lifecycle(&target, Lifecycle::Snooze(ms)).await
        }
        Command::Wake(t) => lifecycle(&t, Lifecycle::Wake).await,
        Command::Pin { off, target } => {
            lifecycle(
                &target,
                if off {
                    Lifecycle::Unpin
                } else {
                    Lifecycle::Pin
                },
            )
            .await
        }
        Command::Archive(t) => lifecycle(&t, Lifecycle::Archive).await,
        Command::Open(t) => {
            let thread = t.resolve().await?;
            install::open_in_t3(&thread).await
        }
        Command::Which(t) => {
            println!("{}", t.resolve().await?);
            Ok(())
        }
        Command::Inbox => install::inbox().await,
        Command::Install => install::install().await,
        Command::Uninstall => install::uninstall().await,
        Command::Doctor => install::doctor().await,
    }
}
