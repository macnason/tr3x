//! t3rex: Rex as the interface to T3 Code. T3 keeps running the agents and owns every thread's
//! state; Rex shows each open thread as a window in one session.

mod actions;
mod ask;
mod chat;
mod compose;
mod draft;
mod fade;
mod find;
mod fold;
mod icons;
mod install;
mod link;
mod machines;
mod media;
mod new;
mod paste;
mod perf;
mod pick;
mod picker;
mod picture;
mod platform;
mod pr;
mod prompt;
mod render;
mod reply;
mod rex;
mod scheme;
mod slash;
mod state;
mod surface;
mod sync;
mod t3;
mod term;
mod update;
mod usage;
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
    /// `KEY=VALUE` for one of t3rex's own settings, as a pane's starter hands them on.
    #[arg(long = "set-env", global = true, hide = true)]
    set_env: Vec<String>,
    #[command(subcommand)]
    command: Command,
}

// What a user runs is install, uninstall, doctor, update, updates, width, details, pair,
// envs, env, sessions and reply.
// The rest is what Rex's actions, panes and config run, so `--help` leaves it out.
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
    /// The Reply pane of a thread's window, when Reply has a pane of its own.
    #[command(hide = true)]
    Compose {
        thread: String,
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
    /// Find Thread: every thread on every machine, to bring one back. The palette's Find
    /// Thread… runs it in a floating layer.
    #[command(hide = true)]
    Find {
        /// The Rex client that asked, which goes to the thread's tab.
        #[arg(long)]
        client: Option<String>,
    },
    /// Bring back the thread settled last, and go to its tab.
    #[command(hide = true)]
    Reopen {
        #[arg(long)]
        client: Option<String>,
    },
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
    /// Give the thread a title. Without one, asks for it.
    #[command(hide = true)]
    Rename {
        title: Option<String>,
        /// Have T3 write a title from the conversation.
        #[arg(long, conflicts_with = "title")]
        regenerate: bool,
        #[command(flatten)]
        target: Target,
    },
    /// Continue the thread in a new one, and go to its tab.
    #[command(hide = true)]
    Fork {
        /// The new thread's title, else T3's.
        title: Option<String>,
        #[arg(long)]
        client: Option<String>,
        #[command(flatten)]
        target: Target,
    },
    /// Delete the thread, in T3 too. Asks first, unless --yes.
    #[command(hide = true)]
    Delete {
        #[arg(long)]
        yes: bool,
        #[command(flatten)]
        target: Target,
    },
    /// Stop the thread, or with --force kill an agent that ignores Stop.
    #[command(hide = true)]
    Stop {
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        target: Target,
    },
    /// Open the thread in T3 Code.
    #[command(hide = true)]
    Open(Target),
    /// Show the thread's pull request in a drawer beside it; --create makes it first.
    #[command(hide = true)]
    Pr {
        /// Commit, push and open the pull request through T3 first.
        #[arg(long)]
        create: bool,
        #[command(flatten)]
        target: Target,
    },
    /// The pull request drawer, as `pr` opens it.
    #[command(hide = true)]
    PrPane {
        thread: String,
        #[arg(long)]
        create: bool,
    },
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
    /// What a T3 build offers: capabilities, commands, RPC methods and keybinding commands.
    /// With --check, what changed since a saved surface; it exits 1 when something did.
    #[command(hide = true)]
    Surface {
        /// A T3 build: its `t3` executable, the desktop app's `app.asar`, or the newest on
        /// this Mac.
        #[arg(long)]
        from: Option<std::path::PathBuf>,
        /// The T3 version it is, to note in the surface.
        #[arg(long)]
        label: Option<String>,
        /// A saved surface to compare with.
        #[arg(long)]
        check: Option<std::path::PathBuf>,
        /// Save the surface here.
        #[arg(long)]
        write: Option<std::path::PathBuf>,
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
    /// Show each finished turn's work (on), or fold it to `Worked for …` (off, the default).
    /// Alone, switches it.
    Details { on: Option<String> },
    /// Pair with T3 on another machine: over SSH, or with a pairing link from T3 there. Alone,
    /// finds the machines on your tailnet that run T3.
    Pair {
        /// A machine's name, an SSH host, or a pairing link.
        machine: Option<String>,
        /// A pairing link for that machine.
        link: Option<String>,
        /// Wait before leaving, so a floating window shows how it went.
        #[arg(long, hide = true)]
        wait: bool,
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
    /// Show or set where Reply goes: inline, under the conversation, or a pane of its own.
    Reply { mode: Option<String> },
    /// Set up t3rex: the Rex config that runs it, and a first sync.
    Install {
        /// As the updater runs it: over the installed t3rex, leaving the session as it is.
        #[arg(long, hide = true)]
        update: bool,
    },
    /// Update t3rex, and T3 on each machine Rex follows. Asks before anything restarts.
    Update {
        /// Only say what's out.
        #[arg(long)]
        check: bool,
        /// As sync runs it: t3rex alone, nothing asked.
        #[arg(long, hide = true)]
        auto: bool,
        /// Wait before leaving, so a floating window shows how it went.
        #[arg(long, hide = true)]
        wait: bool,
    },
    /// Show or set how t3rex updates itself: auto, notify, or off.
    Updates { mode: Option<String> },
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
    // A pane starts again once the binary it runs is replaced; this is the one it started on.
    icons::note_binary();
    for pair in &cli.set_env {
        if let Some((key, value)) = pair.split_once('=')
            && rex::PANE_ENV.contains(&key)
        {
            // SAFETY: no other thread exists yet.
            unsafe { std::env::set_var(key, value) };
        }
    }
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
    lifecycle_of(&target.resolve().await?, action).await
}

async fn lifecycle_of(key: &str, action: Lifecycle) -> Result<()> {
    let (client, thread) = t3::thread(key)?;
    let thread = thread.thread;
    let first = client.dispatch(t3::command(&thread, &action)).await;
    // T3 won't settle or archive a thread mid-run. Settling says the thread is finished, so
    // stop the run, wait for T3 to see it stop, and try again.
    let running = matches!(&first, Err(e) if {
        let text = e.to_string();
        text.contains("still running") || text.contains("waiting on an approval or question")
    });
    if !(running
        && matches!(
            action,
            Lifecycle::Settle | Lifecycle::Archive | Lifecycle::Delete
        ))
    {
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

/// The thread as T3's shell has it: its title and the run it's on.
async fn shell_thread(env: &t3client::env::Env, id: &str) -> Option<serde_json::Value> {
    let shell = env.shell().await.ok()?;
    t3client::inbox::find_thread(&shell, id).cloned()
}

/// In a floating layer the window closes with the process, so a failure stays up a moment.
async fn shown(result: Result<()>) -> Result<()> {
    if let Err(e) = &result {
        println!("\r\n  ✗ {e:#}");
        tokio::time::sleep(Duration::from_secs(4)).await;
    }
    result
}

fn interactive() -> bool {
    // SAFETY: isatty(3) on stdin.
    unsafe { libc::isatty(0) == 1 }
}

async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Sync { no_first, log } => sync::run(!no_first, log).await,
        Command::Chat { thread, passive } => {
            let role = if reply::own_pane() {
                chat::Role::Conversation
            } else {
                chat::Role::Inline
            };
            chat::run(thread, passive, role).await
        }
        Command::Compose { thread, passive } => chat::run(thread, passive, chat::Role::Reply).await,
        Command::Find { client } => find::run(client).await,
        Command::Reopen { client } => find::reopen_last(client).await,
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
        Command::Rename {
            title,
            regenerate,
            target,
        } => {
            let (env, r) = t3::thread(&target.resolve().await?)?;
            if regenerate {
                return t3::rename(&env, &r.thread, None).await;
            }
            if let Some(title) = title {
                return t3::rename(&env, &r.thread, Some(&title)).await;
            }
            let current = shell_thread(&env, &r.thread).await;
            let current = current.as_ref().and_then(|t| t["title"].as_str());
            match prompt::ask("Rename Thread", current.unwrap_or_default(), false).await? {
                prompt::Answer::Text(title) => {
                    shown(t3::rename(&env, &r.thread, Some(&title)).await).await
                }
                _ => Ok(()),
            }
        }
        Command::Fork {
            title,
            client,
            target,
        } => {
            let (env, r) = t3::thread(&target.resolve().await?)?;
            let new = t3::fork(&env, &r.thread, title.as_deref()).await?;
            let key = t3client::hub::ThreadRef::new(env.id(), &new).to_string();
            find::go_to(&key, client.as_deref()).await
        }
        Command::Delete { yes, target } => {
            let key = target.resolve().await?;
            if yes {
                return lifecycle_of(&key, Lifecycle::Delete).await;
            }
            if !interactive() {
                bail!("deleting can't be undone: add --yes");
            }
            let (env, r) = t3::thread(&key)?;
            let current = shell_thread(&env, &r.thread).await;
            let title = current
                .as_ref()
                .and_then(|t| t["title"].as_str())
                .unwrap_or("This thread");
            let subject = format!("{title} goes for good, in T3 too.");
            match prompt::ask("Delete Thread", &subject, true).await? {
                prompt::Answer::Yes => shown(lifecycle_of(&key, Lifecycle::Delete).await).await,
                _ => Ok(()),
            }
        }
        Command::Stop { force, target } => {
            let (env, r) = t3::thread(&target.resolve().await?)?;
            let current = shell_thread(&env, &r.thread).await;
            let run = current
                .as_ref()
                .and_then(|t| t["activeRunId"].as_str())
                .map(str::to_string);
            if force {
                println!("{}", t3::force_stop(&env, &r.thread, run.as_deref()).await?);
                return Ok(());
            }
            if !t3::stop(&env, &r.thread, run.as_deref()).await? {
                bail!("nothing is running");
            }
            Ok(())
        }
        Command::Surface {
            from,
            label,
            check,
            write,
        } => surface::run(from, label, check, write),
        Command::Open(t) => {
            let thread = t.resolve().await?;
            install::open_in_t3(&thread).await
        }
        Command::Pr { create, target } => {
            let thread = target.resolve().await?;
            let block = target.block.context("run this in a T3 window in Rex")?;
            pr::open(&thread, &block, create).await
        }
        Command::PrPane { thread, create } => pr::pane(thread, create).await,
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
        Command::Details { on } => {
            println!("{}", width::apply_details(on.as_deref().unwrap_or(""))?);
            Ok(())
        }
        Command::Width { width } => {
            println!("{}", width::apply(width.as_deref().unwrap_or(""))?);
            Ok(())
        }
        Command::Pair {
            machine,
            link,
            wait,
        } => {
            let result = machines::pair_command(machine, link).await;
            if wait {
                if let Err(e) = &result {
                    println!("✗ {e:#}");
                }
                tokio::time::sleep(Duration::from_secs(if result.is_ok() { 2 } else { 6 })).await;
            }
            result
        }
        Command::Envs => machines::list().await,
        Command::Env { action } => match action {
            EnvAction::Rename { machine, name } => machines::rename(&machine, &name).await,
            EnvAction::Forget { machine } => machines::forget(&machine).await,
        },
        Command::Sessions { split } => {
            println!("{}", machines::set_sessions(split.as_deref())?);
            Ok(())
        }
        Command::Reply { mode } => {
            println!("{}", reply::apply(mode.as_deref().unwrap_or_default())?);
            Ok(())
        }
        Command::Install { update } => install::install(update).await,
        Command::Update { check, auto, wait } => update::run(check, auto, wait).await,
        Command::Updates { mode } => {
            println!(
                "{}",
                update::apply_mode(mode.as_deref().unwrap_or_default())?
            );
            Ok(())
        }
        Command::Uninstall => install::uninstall().await,
        Command::Doctor => install::doctor().await,
    }
}
