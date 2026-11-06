//! What you can do to a thread from Rex, in one list: each entry is a palette action under
//! **T3 Code**, a `/` command in Reply, or both. `install` writes the palette's half into
//! `tr3x.lua` and Reply's `/` menu reads the rest, so a new action is one entry here, plus what
//! it does in `main.rs` (the palette) and `compose.rs` (Reply).
//!
//! Actions that need more than the focused thread (New Thread, Find Thread…, Fit to This
//! Device, pairing, sessions, Open Thread in T3 Code) are written out in `rex/tr3x.lua`.

pub struct Action {
    /// The Rex action's name, `t3_…`, which `rex.bind` takes. Empty: Reply only.
    pub id: &'static str,
    /// The palette's title.
    pub title: &'static str,
    /// More words the palette matches.
    pub keywords: &'static str,
    /// What the palette runs: `tr3x` with these arguments, for the focused thread.
    pub args: &'static [&'static str],
    /// The palette runs it in a floating layer, for an action that asks something first.
    pub layer: bool,
    /// It goes to a tab it makes, on the device that ran it.
    pub client: bool,
    /// Reply's `/` command.
    pub slash: Option<Slash>,
    /// The capability T3 needs for it. Reply leaves it out of `/` on a machine without it, and
    /// the palette says to update T3 there.
    pub needs: Option<&'static str>,
}

pub struct Slash {
    pub name: &'static str,
    pub description: &'static str,
    /// Shown dim after the command once completed, until you type.
    pub hint: Option<&'static str>,
}

const fn slash(
    name: &'static str,
    description: &'static str,
    hint: Option<&'static str>,
) -> Option<Slash> {
    Some(Slash {
        name,
        description,
        hint,
    })
}

const NONE: Action = Action {
    id: "",
    title: "",
    keywords: "",
    args: &[],
    layer: false,
    client: false,
    slash: None,
    needs: None,
};

/// In Reply's `/` order.
pub const ALL: &[Action] = &[
    Action {
        slash: slash("steer", "Send into the running turn", Some("message")),
        ..NONE
    },
    Action {
        slash: slash("clear", "Start a fresh session", None),
        needs: Some("freshContext"),
        ..NONE
    },
    Action {
        id: "t3_settle",
        title: "Settle Thread",
        keywords: "t3 done close archive inbox",
        args: &["settle"],
        slash: slash("settle", "Settle this thread", None),
        ..NONE
    },
    Action {
        slash: slash("unsettle", "Back to the inbox", None),
        ..NONE
    },
    Action {
        slash: slash("snooze", "Snooze this thread", Some("2h, tomorrow")),
        ..NONE
    },
    Action {
        id: "t3_snooze_hour",
        title: "Snooze Thread for 1 Hour",
        keywords: "t3 later",
        args: &["snooze", "1h"],
        ..NONE
    },
    Action {
        id: "t3_snooze_3h",
        title: "Snooze Thread for 3 Hours",
        keywords: "t3 later",
        args: &["snooze", "3h"],
        ..NONE
    },
    Action {
        id: "t3_snooze_tomorrow",
        title: "Snooze Thread Until Tomorrow",
        keywords: "t3 later",
        args: &["snooze", "tomorrow"],
        ..NONE
    },
    Action {
        slash: slash("wake", "End the snooze", None),
        ..NONE
    },
    Action {
        id: "t3_pin",
        title: "Pin Thread",
        keywords: "t3 keep",
        args: &["pin"],
        slash: slash("pin", "Pin this thread", None),
        ..NONE
    },
    Action {
        id: "t3_unpin",
        title: "Unpin Thread",
        keywords: "t3",
        args: &["pin", "--off"],
        slash: slash("unpin", "Unpin this thread", None),
        ..NONE
    },
    Action {
        id: "t3_rename",
        title: "Rename Thread…",
        keywords: "t3 title name",
        args: &["rename"],
        layer: true,
        slash: slash("rename", "Rename this thread", Some("title")),
        ..NONE
    },
    Action {
        id: "t3_retitle",
        title: "Regenerate Thread Title",
        keywords: "t3 rename name title auto",
        args: &["rename", "--regenerate"],
        slash: slash("retitle", "Let T3 title this thread", None),
        needs: Some("threadTitleRegeneration"),
        ..NONE
    },
    Action {
        id: "t3_fork",
        title: "Fork Thread",
        keywords: "t3 copy duplicate branch new",
        args: &["fork"],
        client: true,
        slash: slash("fork", "Continue in a new thread", Some("title")),
        ..NONE
    },
    Action {
        id: "t3_archive",
        title: "Archive Thread",
        keywords: "t3 hide",
        args: &["archive"],
        slash: slash("archive", "Archive this thread", None),
        ..NONE
    },
    Action {
        id: "t3_delete",
        title: "Delete Thread…",
        keywords: "t3 remove trash",
        args: &["delete"],
        layer: true,
        slash: slash("delete", "Delete this thread for good", None),
        ..NONE
    },
    Action {
        id: "t3_stop",
        title: "Stop Thread",
        keywords: "t3 interrupt cancel halt",
        args: &["stop"],
        slash: slash("stop", "Stop the running turn", None),
        ..NONE
    },
    Action {
        id: "t3_force_stop",
        title: "Force Stop Thread",
        keywords: "t3 kill interrupt stuck unresponsive",
        args: &["stop", "--force"],
        slash: slash("force-stop", "Kill an agent that ignores Stop", None),
        needs: Some("threadForceStop"),
        ..NONE
    },
    Action {
        id: "t3_model",
        title: "Change Model…",
        keywords: "t3 effort access provider reasoning",
        args: &["model"],
        slash: slash("model", "Choose the model", None),
        ..NONE
    },
    Action {
        slash: slash("effort", "Choose the effort", None),
        ..NONE
    },
    Action {
        slash: slash("mode", "Choose the access", None),
        ..NONE
    },
    Action {
        slash: slash("open", "Open in T3 Code", None),
        ..NONE
    },
    Action {
        id: "t3_pr",
        title: "Show Pull Request",
        keywords: "t3 pr github review merge checks comments drawer",
        args: &["pr"],
        slash: slash("pr", "Show the pull request", Some("create")),
        ..NONE
    },
    Action {
        id: "t3_pr_create",
        title: "Create Pull Request",
        keywords: "t3 pr github commit push open",
        args: &["pr", "--create"],
        ..NONE
    },
    Action {
        slash: slash(
            "width",
            "Set the chat width",
            Some("comfortable, wide, full, t3"),
        ),
        ..NONE
    },
    Action {
        slash: slash("details", "Show or fold each turn's work", Some("on, off")),
        ..NONE
    },
    Action {
        slash: slash(
            "reply",
            "Reply inline or in its own pane",
            Some("inline, pane"),
        ),
        ..NONE
    },
    Action {
        slash: slash("update", "Update tr3x and T3", None),
        ..NONE
    },
    Action {
        slash: slash(
            "updates",
            "How tr3x updates itself",
            Some("auto, notify, off"),
        ),
        ..NONE
    },
];

/// The `/` commands Reply runs itself.
pub fn slashes() -> impl Iterator<Item = (&'static Slash, Option<&'static str>)> {
    ALL.iter()
        .filter_map(|a| a.slash.as_ref().map(|s| (s, a.needs)))
}

pub fn is_slash(name: &str) -> bool {
    slashes().any(|(s, _)| s.name == name)
}

/// The palette's half, as Lua for `tr3x.lua`.
pub fn lua() -> String {
    let mut out = String::new();
    for a in ALL.iter().filter(|a| !a.id.is_empty()) {
        let args: Vec<String> = a.args.iter().map(|s| quote(s)).collect();
        out += &format!(
            "{}({}, {}, {{ {} }}, {}{})\n",
            if a.layer {
                "layer_action"
            } else {
                "thread_action"
            },
            quote(a.id),
            quote(a.title),
            args.join(", "),
            quote(a.keywords),
            if a.client { ", { client = true }" } else { "" },
        );
    }
    out
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn names_are_unique() {
        let mut ids = HashSet::new();
        let mut slashes = HashSet::new();
        for a in ALL {
            assert!(a.id.is_empty() || ids.insert(a.id), "{}", a.id);
            assert!(a.id.is_empty() || a.id.starts_with("t3_"), "{}", a.id);
            assert!(a.id.is_empty() == a.args.is_empty(), "{}", a.id);
            if let Some(s) = &a.slash {
                assert!(slashes.insert(s.name), "{}", s.name);
            }
            assert!(!a.id.is_empty() || a.slash.is_some());
        }
    }

    #[test]
    fn palette_lua() {
        let lua = lua();
        assert!(lua.contains(
            "thread_action(\"t3_unpin\", \"Unpin Thread\", { \"pin\", \"--off\" }, \"t3\")\n"
        ));
        assert!(lua.contains("layer_action(\"t3_rename\", \"Rename Thread…\", { \"rename\" }"));
        assert!(lua.contains("{ \"fork\" }, \"t3 copy duplicate branch new\", { client = true })"));
        assert!(!lua.contains("steer"));
    }

    /// Every palette action is a tr3x command main.rs knows.
    #[test]
    fn palette_args_parse() {
        use clap::CommandFactory;
        let cli = crate::Cli::command();
        for a in ALL.iter().filter(|a| !a.id.is_empty()) {
            let argv = ["tr3x"]
                .into_iter()
                .chain(a.args.iter().copied())
                .chain(["--block", "b1"]);
            if a.client {
                let argv: Vec<&str> = argv.chain(["--client", "c1"]).collect();
                cli.clone().try_get_matches_from(argv).unwrap();
            } else {
                cli.clone().try_get_matches_from(argv).unwrap();
            }
        }
    }
}
