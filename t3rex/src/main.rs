//! t3rex: Rex as the interface to T3 Code. T3 keeps running the agents and owns every thread's
//! state; Rex shows each open thread as a window in one session.

mod ask;
mod chat;
mod compose;
mod fade;
mod icons;
mod install;
mod link;
mod machines;
mod media;
mod new;
mod pick;
mod picker;
mod render;
mod rex;
mod scheme;
mod slash;
mod state;
mod sync;
mod t3;
mod width;

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

// What a user runs is install, uninstall, doctor, width, pair, envs, env and sessions. The rest is what Rex's actions,
// panes and config run, so `--help` leaves it out.
#[derive(Subcommand)]
enum Command {
    /// Keep the "T3 Code" Rex session in step with T3. Runs until stopped; Rex starts it.
    #[command(hide = true)]
    Sync {
        /// Leave the session where Rex puts new sessions, instead of first.
        #[arg(long)]
        no_first: bool,
        /// Write to sync.log in t3rex's state directory, as when Rex starts it.
        #[arg(long)]
        log: bool,
    },
    /// The conversation pane of a thread's window.
    #[command(hide = true)]
    Chat {
        thread: String,
        /// Only read: never mark the thread visited. For a development sync beside an
        /// installed one.
        #[arg(long)]
        passive: bool,
    },
    /// A new thread: the window Rex's New Tab makes in the T3 Code session.
    #[command(hide = true)]
    New {
        /// Show the keys that matter, for a session with no threads yet.
        #[arg(long)]
        welcome: bool,
        /// The machine to start on.
        #[arg(long)]
        env: Option<String>,
    },
    /// The model picker, as the floating layer `model` opens runs it.
    #[command(hide = true)]
    Pick {
        thread: String,
        #[arg(long, value_enum, default_value = "model")]
        field: pick::Field,
    },
    /// Choose the model, effort and access for the thread's next message, in a floating layer.
    #[command(hide = true)]
    Model(Target),
    /// The reply pane of a thread's window.
    #[command(hide = true)]
    Compose { thread: String },
    /// Print a thread's transcript blocks, for working on the chat pane.
    #[command(hide = true)]
    Blocks { thread: String },
    /// Settle the thread. Its window closes.
    #[command(hide = true)]
    Settle(Target),
    /// Bring a settled thread back.
    #[command(hide = true)]
    Unsettle(Target),
    /// Snooze the thread, for 2h, 30m, 1d and so on.
    #[command(hide = true)]
    Snooze {
        #[arg(default_value = "1h")]
        duration: String,
        #[command(flatten)]
        target: Target,
    },
    /// End the thread's snooze.
    #[command(hide = true)]
    Wake(Target),
    /// Pin the thread, or unpin it with --off.
    #[command(hide = true)]
    Pin {
        #[arg(long)]
        off: bool,
        #[command(flatten)]
        target: Target,
    },
    /// Archive the thread.
    #[command(hide = true)]
    Archive(Target),
    /// Open the thread in T3 Code.
    #[command(hide = true)]
    Open(Target),
    /// Call a T3 RPC method and print its answer, for development.
    #[command(hide = true)]
    Rpc {
        method: String,
        #[arg(default_value = "{}")]
        payload: String,
        /// The machine to call, else this Mac.
        #[arg(long)]
        env: Option<String>,
    },
    /// Print the windows sync wants, in order, for working on sync.
    #[command(hide = true)]
    Wants,
    /// Print where the thread that most needs you is, as Rex's session.select takes it.
    #[command(hide = true)]
    Inbox,
    /// Print the thread a Rex block belongs to.
    #[command(hide = true)]
    Which(Target),
    /// Show or set how wide conversations read: comfortable, wide, full, a column count, or
    /// t3 to follow T3 Code's Chat width.
    Width { width: Option<String> },
    /// Pair with T3 on another machine: over SSH, or with a pairing link from T3 there. Alone,
    /// finds the machines on your tailnet that run T3.
    Pair {
        /// A machine's name, an SSH host, or a pairing link.
        machine: Option<String>,
        /// A pairing link for that machine.
        link: Option<String>,
    },
    /// List the machines Rex follows.
    Envs,
    /// Rename or forget a machine.
    Env {
        #[command(subcommand)]
        action: EnvAction,
    },
    /// Show or set how the T3 Code session holds your machines: merged or per-machine.
    Sessions { split: Option<String> },
    /// Set up t3rex: the Rex config that runs it, and a first sync.
    Install,
    /// Undo `install`: stop sync, remove the config, close t3rex's session.
    Uninstall,
    /// Check each part of the setup, with a fix for what's wrong.
    Doctor,
}

#[derive(Subcommand)]
enum EnvAction {
    /// Call a machine something else in Rex. An empty name goes back to T3's.
    Rename { machine: String, name: String },
    /// Unpair a machine.
    Forget { machine: String },
}

/// Which thread a command acts on. Without flags, the thread of the Rex block it runs in.
#[derive(clap::Args, Clone)]
struct Target {
    /// A T3 thread, `env/thread`, or a thread id on this Mac.
    #[arg(long)]
    thread: Option<String>,
    /// A Rex block in a t3rex window, as Rex actions pass `ctx.block_id`.
    #[arg(long, env = "REX_BLOCK")]
    block: Option<String>,
}

impl Target {
    /// The thread, `env/thread`.
    async fn resolve(&self) -> Result<String> {
        if let Some(thread) = &self.thread {
            return Ok(t3::thread(thread)?.1.to_string());
        }
        let Some(block) = &self.block else {
            bail!("name a thread with --thread, or run this in a T3 window in Rex");
        };
        let state = State::load();
        let mut sessions = state.sessions();
        if sessions.is_empty() {
            bail!("t3rex hasn't synced yet");
        }
        // The session the action ran in first.
        if let Ok(here) = std::env::var("REX_SESSION")
            && let Some(at) = sessions.iter().position(|s| *s == here)
        {
            sessions.swap(0, at);
        }
        for session in sessions {
            let Ok(view) = Rex::find().inspect(&session).await else {
                continue;
            };
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
                .and_then(|w| w["window_id"].as_str());
            if let Some(window) = window {
                return state
                    .thread_for_window(window)
                    .map(str::to_string)
                    .context("that window isn't one of t3rex's");
            }
        }
        bail!("that block isn't in a T3 window")
    }
}

fn main() {
    // A pane Rex starts without a login shell gets Rex's PATH alone; pairing runs `ps`,
    // `ssh` and T3's own command, so the system folders go on the end.
    let path = std::env::var("PATH").unwrap_or_default();
    let missing: Vec<&str> = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"]
        .into_iter()
        .filter(|dir| !path.split(':').any(|p| p == *dir))
        .collect();
    if !missing.is_empty() {
        let joined = [path.as_str()]
            .into_iter()
            .filter(|p| !p.is_empty())
            .chain(missing)
            .collect::<Vec<_>>()
            .join(":");
        // SAFETY: no other thread exists yet.
        unsafe { std::env::set_var("PATH", joined) };
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
    scheme::stop();
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
    let (client, thread) = t3::thread(&target.resolve().await?)?;
    let thread = thread.thread;
    let first = client.dispatch(t3::command(&thread, &action)).await;
    // T3 won't settle or archive a thread mid-run. Settling says the thread is finished, so
    // stop the run, wait for T3 to see it stop, and try again.
    let running = matches!(&first, Err(e) if {
        let text = e.to_string();
        text.contains("still running") || text.contains("waiting on an approval or question")
    });
    if !(running && matches!(action, Lifecycle::Settle | Lifecycle::Archive)) {
        return first.map(drop);
    }
    let run_id = |shell: &t3client::projection::ShellState| {
        t3client::inbox::find_thread(shell, &thread)
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
        Command::Sync { no_first, log } => sync::run(!no_first, log).await,
        Command::Chat { thread, passive } => chat::run(thread, passive).await,
        Command::Compose { thread } => compose::run(thread).await,
        Command::New { welcome, env } => new::run(welcome, env).await,
        Command::Pick { thread, field } => pick::run(thread, field).await,
        Command::Model(t) => {
            let thread = t.resolve().await?;
            let session = std::env::var("REX_SESSION")
                .ok()
                .or(State::load().session())
                .context("t3rex hasn't synced yet")?;
            pick::float(&session, &thread, pick::Field::Model).await
        }
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
        Command::Rpc {
            method,
            payload,
            env,
        } => {
            let client = t3::machine(env.as_deref().unwrap_or(""))?;
            let answer = client
                .rpc()
                .await?
                .call(
                    &method,
                    serde_json::from_str(&payload)?,
                    Duration::from_secs(20),
                )
                .await?;
            println!("{}", serde_json::to_string_pretty(&answer)?);
            Ok(())
        }
        Command::Wants => {
            let hub = t3::hub();
            // The same feeds sync follows, each up to its first synchronized state.
            let mut shells = Vec::new();
            for env in hub.envs() {
                let mut events = env.watch_shell(None);
                let mut shell = t3client::projection::ShellState::default();
                let caught_up = tokio::time::timeout(Duration::from_secs(10), async {
                    while let Some(t3client::env::WatchEvent::Item(item)) = events.recv().await {
                        shell.apply(&item);
                        if item["kind"] == "synchronized" {
                            return true;
                        }
                    }
                    false
                })
                .await;
                match caught_up {
                    Ok(true) => shells.push((env.clone(), shell)),
                    _ => eprintln!("{} didn't answer", env.name()),
                }
            }
            let home = hub.home_id().unwrap_or_default().to_string();
            let badges = hub.paired() >= 2;
            let machines: Vec<sync::MachineShell> = shells
                .iter()
                .map(|(env, shell)| sync::MachineShell {
                    env: env.id(),
                    badge: (badges && env.id() != home).then(|| env.name()),
                    local: env.is_local(),
                    shell,
                    caps: t3::capabilities(env),
                    offline: false,
                })
                .collect();
            let mut inbox = t3client::inbox::Inbox::default();
            for want in sync::wanted(&machines, &mut inbox, t3::now_ms()) {
                println!("{:?}\t{}\t{}", want.place, want.thread, want.label);
            }
            Ok(())
        }
        Command::Width { width } => {
            println!("{}", width::apply(width.as_deref().unwrap_or(""))?);
            Ok(())
        }
        Command::Pair { machine, link } => machines::pair_command(machine, link).await,
        Command::Envs => machines::list().await,
        Command::Env { action } => match action {
            EnvAction::Rename { machine, name } => machines::rename(&machine, &name).await,
            EnvAction::Forget { machine } => machines::forget(&machine).await,
        },
        Command::Sessions { split } => {
            println!("{}", machines::set_sessions(split.as_deref())?);
            Ok(())
        }
        Command::Install => install::install().await,
        Command::Uninstall => install::uninstall().await,
        Command::Doctor => install::doctor().await,
    }
}
