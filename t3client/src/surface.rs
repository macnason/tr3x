//! What a T3 build offers a client: its capability flags, the orchestration commands it takes,
//! its RPC methods and the commands its keybindings and command palette run. Read from the
//! server's own source, which every T3 build carries as text: the `t3` executable a runtime
//! install unpacks, the desktop app's `app.asar`, or the release tarball CI downloads.
//!
//! t3rex keeps the surface it was last checked against in `t3rex/t3-surface.json`. Comparing a
//! newer build with it says what T3 added (actions t3rex could offer) and removed (calls t3rex
//! may make that would now fail).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Surface {
    /// The T3 version this was read from, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t3: Option<String>,
    /// `ExecutionEnvironmentCapabilities`: the flags a machine's descriptor can carry.
    pub capabilities: BTreeSet<String>,
    /// `OrchestrationV2Command` and `ProjectMutation`: what `orchestration.dispatchCommand`
    /// and `projects.mutate` take.
    pub commands: BTreeSet<String>,
    /// `ORCHESTRATION_V2_WS_METHODS` and `WS_METHODS`: the RPC methods.
    pub methods: BTreeSet<String>,
    /// The keybinding commands, which are also what T3's command palette runs.
    pub keybindings: BTreeSet<String>,
}

/// Where each list lives in T3's source (`packages/contracts`). The desktop app's bundle
/// declares it at the top level, `const Name = …`; the executable's declares it inside a
/// module's initialiser, a tab further in. Either way its entries sit one or two tabs deeper
/// than the line that opens it, and it closes at that line's depth.
struct Section {
    /// The declaration, from the name to its opening bracket.
    start: &'static str,
    /// How many tabs deeper than `start` its entries are. 0: any, for a list that spreads
    /// another into itself.
    depth: usize,
    take: Take,
}

/// What an entry's line holds.
#[derive(Clone, Copy)]
enum Take {
    /// `type: Literal("name"),`
    Literal,
    /// `name: Schema,`
    Key,
    /// `key: "name",`
    Value,
    /// `"name",`
    Item,
}

const CAPABILITIES: &[Section] = &[Section {
    start: "ExecutionEnvironmentCapabilities = Struct({",
    depth: 1,
    take: Take::Key,
}];
const COMMANDS: &[Section] = &[
    Section {
        start: "OrchestrationV2Command = Union([",
        depth: 2,
        take: Take::Literal,
    },
    Section {
        start: "ProjectMutation = Union([",
        depth: 2,
        take: Take::Literal,
    },
];
const METHODS: &[Section] = &[
    Section {
        start: "ORCHESTRATION_V2_WS_METHODS = {",
        depth: 1,
        take: Take::Value,
    },
    Section {
        start: "WS_METHODS = {",
        depth: 1,
        take: Take::Value,
    },
];
const KEYBINDINGS: &[Section] = &[
    Section {
        start: "STATIC_KEYBINDING_COMMANDS = [",
        depth: 0,
        take: Take::Item,
    },
    Section {
        start: "MODEL_PICKER_KEYBINDING_COMMANDS = [",
        depth: 0,
        take: Take::Item,
    },
    Section {
        start: "THREAD_KEYBINDING_COMMANDS = [",
        depth: 0,
        take: Take::Item,
    },
];

impl Surface {
    /// Reads the surface from a T3 build's bytes. A list that isn't there is an error: T3 moved
    /// it, and this needs to learn where.
    pub fn read(bytes: &[u8]) -> Result<Surface> {
        let mut missing = Vec::new();
        let mut take = |sections: &[Section], name: &str| {
            let found = collect(bytes, sections);
            if found.is_empty() {
                missing.push(name.to_string());
            }
            found
        };
        let surface = Surface {
            t3: None,
            capabilities: take(CAPABILITIES, "capabilities"),
            commands: take(COMMANDS, "commands"),
            methods: take(METHODS, "methods"),
            keybindings: take(KEYBINDINGS, "keybindings"),
        };
        if !missing.is_empty() {
            bail!(
                "this T3 build doesn't have its {} where t3rex looks for them",
                missing.join(", ")
            );
        }
        Ok(surface)
    }

    pub fn from_file(path: &Path) -> Result<Surface> {
        let bytes =
            std::fs::read(path).with_context(|| format!("can't read {}", path.display()))?;
        Surface::read(&bytes).with_context(|| path.display().to_string())
    }

    /// What `newer` has that this doesn't, and the other way round, list by list.
    pub fn diff(&self, newer: &Surface) -> Vec<Change> {
        let lists = [
            ("capabilities", &self.capabilities, &newer.capabilities),
            ("commands", &self.commands, &newer.commands),
            ("methods", &self.methods, &newer.methods),
            ("keybindings", &self.keybindings, &newer.keybindings),
        ];
        let mut out = Vec::new();
        for (list, old, new) in lists {
            for name in new.difference(old) {
                out.push(Change::new(list, name, true));
            }
            for name in old.difference(new) {
                out.push(Change::new(list, name, false));
            }
        }
        out
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub list: &'static str,
    pub name: String,
    pub added: bool,
}

impl Change {
    fn new(list: &'static str, name: &str, added: bool) -> Change {
        Change {
            list,
            name: name.to_string(),
            added,
        }
    }
}

/// Every name in every copy of these sections. A build can carry its source more than once.
fn collect(bytes: &[u8], sections: &[Section]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for section in sections {
        let start = section.start.as_bytes();
        let mut from = 0;
        while let Some(at) = find(&bytes[from..], start) {
            let at = from + at;
            from = at + start.len();
            // Its own line, after nothing but indentation: not `XWS_METHODS` or a use of it.
            let line = bytes[..at]
                .iter()
                .rposition(|&b| b == b'\n')
                .map_or(0, |n| n + 1);
            let lead = &bytes[line..at];
            let indent = lead.strip_prefix(b"const ").unwrap_or(lead);
            if !indent.iter().all(|&b| b == b'\t') {
                continue;
            }
            let (indent, inner) = (indent.len(), indent.len() + section.depth);
            for line in bytes[from..].split(|&b| b == b'\n').skip(1) {
                let tabs = line.iter().take_while(|&&b| b == b'\t').count();
                if tabs <= indent {
                    break;
                }
                if (section.depth == 0 || tabs == inner)
                    && let Some(name) = entry(&line[tabs..], section.take)
                {
                    out.insert(name);
                }
            }
        }
    }
    out
}

fn quoted(rest: &[u8]) -> Option<&[u8]> {
    let len = rest.iter().position(|&b| b == b'"')?;
    Some(&rest[..len])
}

/// The name on an entry's line, past its indentation.
fn entry(line: &[u8], take: Take) -> Option<String> {
    let name = match take {
        Take::Literal => quoted(line.strip_prefix(b"type: Literal(\"")?)?,
        Take::Key => {
            let len = line.iter().position(|&b| b == b':')?;
            &line[..len]
        }
        Take::Value => {
            let at = find(line, b": \"")?;
            quoted(&line[at + 3..])?
        }
        Take::Item => quoted(line.strip_prefix(b"\"")?)?,
    };
    let fine = |b: &u8| b.is_ascii_alphanumeric() || b"._-/$".contains(b);
    (!name.is_empty() && name.iter().all(fine)).then(|| String::from_utf8_lossy(name).into_owned())
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    let (&first, tail) = needle.split_first()?;
    let mut from = 0;
    while let Some(at) = hay[from..].iter().position(|&b| b == first) {
        let start = from + at;
        let end = start + needle.len();
        if end > hay.len() {
            return None;
        }
        if &hay[start + 1..end] == tail {
            return Some(start);
        }
        from = start + 1;
    }
    None
}

/// The T3 builds on this machine, newest first: the runtimes T3's CLI installs and the
/// desktop apps.
pub fn installed() -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    if let Ok(versions) = std::fs::read_dir(crate::descriptor::t3_home().join("runtime/versions")) {
        found.extend(
            versions
                .flatten()
                .map(|v| v.path().join("t3"))
                .filter(|p| p.is_file()),
        );
    }
    if let Ok(apps) = std::fs::read_dir("/Applications") {
        found.extend(
            apps.flatten()
                .filter(|a| a.file_name().to_string_lossy().starts_with("T3 Code"))
                .map(|a| a.path().join("Contents/Resources/app.asar"))
                .filter(|p| p.is_file()),
        );
    }
    let modified = |p: &PathBuf| p.metadata().and_then(|m| m.modified()).ok();
    found.sort_by_key(|p| std::cmp::Reverse(modified(p)));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = concat!(
        "noise\0\u{1}\nconst ExecutionEnvironmentCapabilities = Struct({\n",
        "\trepositoryIdentity: Boolean$5.pipe(withDecodingDefault(succeed$1(false))),\n",
        "\tfileAttachments: optionalKey(Struct({ maxUploadBytes: Int$8 })),\n",
        "\tthreadSnooze: optionalKey(Boolean$5)\n",
        "});\n",
        "const OrchestrationV2Command = Union([\n",
        "\tStruct({\n\t\ttype: Literal(\"thread.stop\"),\n\t\tcommandId: CommandId\n\t}),\n",
        "\tStruct({\n\t\ttype: Literal(\"thread.fork\"),\n\t\tsourcePoint: Struct({ type: Literal(\"run\") })\n\t})\n",
        "]);\n",
        "const ORCHESTRATION_V2_WS_METHODS = {\n",
        "\tsubscribeShell: \"orchestration.subscribeShell\",\n",
        "\tforceStopThread: \"orchestration.forceStopThread\"\n",
        "};\n",
        "const WS_METHODS = {\n\tassetsCreateUrl: \"assets.createUrl\"\n};\n",
        "const STATIC_KEYBINDING_COMMANDS = [\n\t\"sidebar.toggle\",\n\t...[\n\t\t\"modelPicker.toggle\"\n\t],\n\t...THREAD_KEYBINDING_COMMANDS\n];\n",
        "const THREAD_KEYBINDING_COMMANDS = [\n\t\"thread.settle\",\n\t...THREAD_JUMP_KEYBINDING_COMMANDS\n];\n",
    );

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn reads_each_list() {
        let s = Surface::read(SOURCE.as_bytes()).unwrap();
        assert_eq!(
            s.capabilities,
            set(&["fileAttachments", "repositoryIdentity", "threadSnooze"])
        );
        assert_eq!(s.commands, set(&["thread.fork", "thread.stop"]));
        assert_eq!(
            s.methods,
            set(&[
                "assets.createUrl",
                "orchestration.forceStopThread",
                "orchestration.subscribeShell"
            ])
        );
        assert_eq!(
            s.keybindings,
            set(&["modelPicker.toggle", "sidebar.toggle", "thread.settle"])
        );
    }

    #[test]
    fn a_second_copy_adds_to_the_first() {
        let more = format!(
            "{SOURCE}const OrchestrationV2Command = Union([\n\tStruct({{\n\t\ttype: Literal(\"thread.delete\")\n\t}})\n]);"
        );
        let s = Surface::read(more.as_bytes()).unwrap();
        assert!(s.commands.contains("thread.delete") && s.commands.contains("thread.stop"));
    }

    /// The executable's bundle declares each list inside a module's initialiser.
    #[test]
    fn reads_a_list_a_tab_further_in() {
        let nested = "var init = () => {\n\tOrchestrationV2Command = Union([\n\t\tStruct({\n\t\t\ttype: Literal(\"thread.settle\"),\n\t\t\tsourcePoint: Union([\n\t\t\t\tStruct({\n\t\t\t\t\ttype: Literal(\"run\")\n\t\t\t\t})\n\t\t\t])\n\t\t})\n\t]);\n\tOther = Union([\n\t\tStruct({\n\t\t\ttype: Literal(\"not.this\")\n\t\t})\n\t]);\n};";
        let mut found = collect(nested.as_bytes(), COMMANDS);
        assert_eq!(found.pop_first().as_deref(), Some("thread.settle"));
        assert!(found.is_empty(), "{found:?}");
        // A mention isn't a declaration.
        let mention = "export { OrchestrationV2Command = Union([\n\t\ttype: Literal(\"x.y\")\n]);";
        assert!(collect(mention.as_bytes(), COMMANDS).is_empty());
    }

    #[test]
    fn a_moved_list_is_an_error() {
        let moved = SOURCE
            .replace("const WS_METHODS", "const WsMethods")
            .replace(
                "const ORCHESTRATION_V2_WS_METHODS",
                "const OrchestrationMethods",
            );
        let e = Surface::read(moved.as_bytes()).unwrap_err().to_string();
        assert!(e.contains("methods") && !e.contains("commands"), "{e}");
    }

    #[test]
    fn diffs_both_ways() {
        let old = Surface::read(SOURCE.as_bytes()).unwrap();
        let mut new = old.clone();
        new.commands.insert("thread.delete".into());
        new.capabilities.remove("threadSnooze");
        assert_eq!(
            old.diff(&new),
            [
                Change::new("capabilities", "threadSnooze", false),
                Change::new("commands", "thread.delete", true),
            ]
        );
    }
}
