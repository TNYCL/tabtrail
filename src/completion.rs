//! Pure completion engine: walks a command schema along the already typed
//! arguments and produces candidates for the word under the cursor.
//!
//! No I/O happens here. Dynamic values are requested through `ValueSlot`,
//! which the caller can hand to a provider.

use std::collections::HashSet;

use crate::context::CommandContext;
use crate::schema::{CommandNode, Flag, NodeStatus, ValueKind, ValueSpec};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CandidateKind {
    Subcommand,
    Flag,
    Value,
}

impl CandidateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CandidateKind::Subcommand => "subcommand",
            CandidateKind::Flag => "flag",
            CandidateKind::Value => "value",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// Logical (unquoted) text that replaces the current word from
    /// `Outcome::replace_from` up to the cursor.
    pub value: String,
    pub description: Option<String>,
    pub kind: CandidateKind,
    /// Do not append a space after insertion.
    pub no_space: bool,
}

/// Where a value is expected, for dynamic providers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlotKind {
    FlagValue(String),
    Positional(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueSlot {
    /// Subcommand path below the root command.
    pub path: Vec<String>,
    pub kind: SlotKind,
    /// Logical prefix being completed.
    pub prefix: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub candidates: Vec<Candidate>,
    /// Character index into the current word's logical value where the
    /// replacement starts (non-zero for `--flag=value`).
    pub replace_from: usize,
    /// The shell's file completion is appropriate at this position.
    pub files: bool,
    /// Metadata for this subcommand path is missing and could be discovered.
    pub discover: Option<Vec<String>>,
    /// A dynamic value could be supplied here.
    pub slot: Option<ValueSlot>,
}

impl Outcome {
    fn fallback() -> Self {
        Outcome {
            files: true,
            ..Default::default()
        }
    }
}

fn lookup_flag<'a>(stack: &[&'a CommandNode], name: &str) -> Option<&'a Flag> {
    stack.iter().rev().find_map(|n| n.flag(name))
}

struct Walk<'a> {
    stack: Vec<&'a CommandNode>,
    path: Vec<String>,
    positional: usize,
    after_dd: bool,
    pending: Option<&'a Flag>,
    used: HashSet<String>,
    uncertain: bool,
}

fn walk<'a>(root: &'a CommandNode, ctx: &CommandContext) -> Walk<'a> {
    let mut w = Walk {
        stack: vec![root],
        path: Vec::new(),
        positional: 0,
        after_dd: false,
        pending: None,
        used: HashSet::new(),
        uncertain: false,
    };
    for arg in &ctx.args {
        if w.pending.take().is_some() {
            continue;
        }
        let v = arg.value.as_str();
        let node = *w.stack.last().unwrap_or(&root);
        if w.after_dd {
            w.positional += 1;
            continue;
        }
        if arg.expansion {
            if !node.subcommands.is_empty() && w.positional == 0 {
                // Could be any subcommand: do not guess.
                w.uncertain = true;
                break;
            }
            w.positional += 1;
            continue;
        }
        if v == "--" {
            w.after_dd = true;
            continue;
        }
        if v.starts_with("--") {
            let (name, attached) = match v.split_once('=') {
                Some((n, _)) => (n, true),
                None => (v, false),
            };
            if let Some(f) = lookup_flag(&w.stack, name) {
                w.used.insert(f.names[0].clone());
                if !attached && f.takes_separate_value() {
                    w.pending = Some(f);
                }
            }
            continue;
        }
        if v.starts_with('-') && v.len() > 1 {
            let (name, attached) = match v.split_once('=') {
                Some((n, _)) => (n, true),
                None => (v, false),
            };
            if let Some(f) = lookup_flag(&w.stack, name) {
                w.used.insert(f.names[0].clone());
                if !attached && f.takes_separate_value() {
                    w.pending = Some(f);
                }
                continue;
            }
            // Short option cluster such as -abc or -ofile.
            let chars: Vec<char> = v[1..].chars().collect();
            for (i, c) in chars.iter().enumerate() {
                let Some(f) = lookup_flag(&w.stack, &format!("-{c}")) else {
                    break;
                };
                w.used.insert(f.names[0].clone());
                if f.takes_separate_value() {
                    if i + 1 == chars.len() {
                        w.pending = Some(f);
                    }
                    break;
                }
            }
            continue;
        }
        if w.positional == 0
            && let Some(sub) = node.subcommand(v)
        {
            w.path.push(sub.name.clone());
            w.stack.push(sub);
            w.used.clear();
            continue;
        }
        w.positional += 1;
    }
    w
}

fn value_candidates(spec: &ValueSpec, prefix: &str) -> Vec<Candidate> {
    spec.choices
        .iter()
        .filter(|c| c.value.starts_with(prefix))
        .map(|c| Candidate {
            value: c.value.clone(),
            description: c.description.clone(),
            kind: CandidateKind::Value,
            no_space: false,
        })
        .collect()
}

fn files_for(kind: &ValueKind) -> bool {
    matches!(kind, ValueKind::Any | ValueKind::Path | ValueKind::Directory)
}

/// Computes completions. `root` is `None` when nothing is known about the
/// command yet (cold cache, untrusted, or unsupported).
pub fn complete(root: Option<&CommandNode>, ctx: &CommandContext) -> Outcome {
    let cur = &ctx.current;
    if cur.expansion {
        // Never hand a word containing a command substitution to the shell's
        // file completion: Zsh's `_files` evaluates it (verified with plain
        // Zsh 5.9), which would run code on Tab.
        return Outcome {
            files: !cur.subst,
            discover: root.is_none().then(Vec::new),
            ..Outcome::fallback()
        };
    }
    let Some(root) = root else {
        return Outcome {
            discover: Some(Vec::new()),
            ..Outcome::fallback()
        };
    };
    let w = walk(root, ctx);
    if w.uncertain {
        return Outcome::fallback();
    }
    let node = *w.stack.last().unwrap_or(&root);
    let discover = (node.status == NodeStatus::Listed).then(|| w.path.clone());
    let prefix = cur.value.as_str();

    // Value of a flag given as a separate word.
    if let Some(flag) = w.pending {
        let spec = flag.value.clone().unwrap_or_default();
        return Outcome {
            candidates: value_candidates(&spec, prefix),
            replace_from: 0,
            files: files_for(&spec.kind),
            discover,
            slot: Some(ValueSlot {
                path: w.path.clone(),
                kind: SlotKind::FlagValue(flag.names[0].clone()),
                prefix: prefix.to_string(),
            }),
        };
    }

    if !w.after_dd && prefix.starts_with('-') {
        // --flag=value (also Go-style -flag=value)
        if let Some((name, value_prefix)) = prefix.split_once('=') {
            let Some(flag) = lookup_flag(&w.stack, name) else {
                return Outcome {
                    discover,
                    ..Outcome::fallback()
                };
            };
            let spec = flag.value.clone().unwrap_or_default();
            return Outcome {
                candidates: value_candidates(&spec, value_prefix),
                replace_from: name.chars().count() + 1,
                files: flag.value.is_some() && files_for(&spec.kind),
                discover,
                slot: flag.value.as_ref().map(|_| ValueSlot {
                    path: w.path.clone(),
                    kind: SlotKind::FlagValue(flag.names[0].clone()),
                    prefix: value_prefix.to_string(),
                }),
            };
        }
        let mut seen = HashSet::new();
        let mut candidates = Vec::new();
        for flag in &node.flags {
            if w.used.contains(&flag.names[0]) && !flag.repeatable {
                continue;
            }
            for name in &flag.names {
                let short = name.len() == 2 && !name.starts_with("--");
                // A lone "-" lists everything; "--x" lists long names only.
                let wanted = name.starts_with(prefix) && (prefix.len() > 1 || prefix == "-" || !short);
                if wanted && seen.insert(name.clone()) {
                    candidates.push(Candidate {
                        value: name.clone(),
                        description: flag.description.clone(),
                        kind: CandidateKind::Flag,
                        no_space: false,
                    });
                }
            }
        }
        return Outcome {
            candidates,
            replace_from: 0,
            files: false,
            discover,
            slot: None,
        };
    }

    let mut candidates = Vec::new();
    let at_subcommand = w.positional == 0 && !w.after_dd && !node.subcommands.is_empty();
    if at_subcommand {
        for sub in &node.subcommands {
            let matched = if sub.name.starts_with(prefix) {
                Some(&sub.name)
            } else {
                sub.aliases.iter().find(|a| a.starts_with(prefix))
            };
            if let Some(name) = matched {
                candidates.push(Candidate {
                    value: name.clone(),
                    description: sub.description.clone(),
                    kind: CandidateKind::Subcommand,
                    no_space: false,
                });
            }
        }
    }
    let positional = node
        .positionals
        .get(w.positional)
        .or_else(|| node.positionals.last().filter(|p| p.variadic));
    let files = match positional {
        Some(p) => {
            candidates.extend(value_candidates(&p.value, prefix));
            files_for(&p.value.kind)
        }
        // A node that only dispatches to subcommands takes no file here;
        // otherwise nothing proves that a file is not acceptable.
        None => !(at_subcommand && node.positionals.is_empty() && node.status == NodeStatus::Described),
    };
    Outcome {
        candidates,
        replace_from: 0,
        files,
        discover,
        slot: Some(ValueSlot {
            path: w.path.clone(),
            kind: SlotKind::Positional(w.positional),
            prefix: prefix.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{Context, parse};
    use crate::help::parse_help;
    use crate::shellword::Dialect;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!("{}/tests/fixtures/help/dev/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    fn deployer() -> CommandNode {
        let mut root = parse_help(&fixture("clap_root.txt"), &["deployer"]);
        let deploy = parse_help(&fixture("clap_deploy.txt"), &["deployer", "deploy"]);
        let slot = root.subcommand_mut("deploy").unwrap();
        let description = slot.description.clone();
        *slot = CommandNode { description, ..deploy };
        root
    }

    fn run(root: Option<&CommandNode>, line: &str) -> Outcome {
        match parse(line, Dialect::Bash) {
            Context::Command(c) => complete(root, &c),
            other => panic!("{other:?}"),
        }
    }

    fn values(o: &Outcome) -> Vec<&str> {
        o.candidates.iter().map(|c| c.value.as_str()).collect()
    }

    #[test]
    fn unknown_command_falls_back_and_requests_discovery() {
        let o = run(None, "tool --fl");
        assert!(o.candidates.is_empty());
        assert!(o.files);
        assert_eq!(o.discover, Some(vec![]));
    }

    #[test]
    fn subcommands_at_root() {
        let r = deployer();
        let o = run(Some(&r), "deployer ");
        assert_eq!(values(&o), ["deploy", "rollback", "status", "help"]);
        assert!(!o.files, "pure dispatcher position should not offer files");
        let o = run(Some(&r), "deployer r");
        assert_eq!(values(&o), ["rollback"]);
        assert_eq!(
            o.candidates[0].description.as_deref(),
            Some("Roll back to a previous release")
        );
    }

    #[test]
    fn flags_at_root_and_nested() {
        let r = deployer();
        assert_eq!(values(&run(Some(&r), "deployer --c")), ["--config", "--color"]);
        let o = run(Some(&r), "deployer deploy --");
        assert_eq!(values(&o), ["--env", "--dry-run", "--tag", "--jobs", "--help"]);
        // single dash lists short and long names
        assert!(values(&run(Some(&r), "deployer deploy -")).contains(&"-e"));
    }

    #[test]
    fn separate_and_attached_flag_values() {
        let r = deployer();
        let o = run(Some(&r), "deployer deploy --env ");
        assert_eq!(values(&o), ["dev", "staging", "prod"]);
        assert!(!o.files);
        assert_eq!(o.slot.as_ref().unwrap().kind, SlotKind::FlagValue("-e".into()));
        let o = run(Some(&r), "deployer deploy -e st");
        assert_eq!(values(&o), ["staging"]);
        let o = run(Some(&r), "deployer deploy --env=pr");
        assert_eq!(values(&o), ["prod"]);
        assert_eq!(o.replace_from, "--env=".len());
        // a path-typed value keeps file completion
        let o = run(Some(&r), "deployer --config ");
        assert!(o.candidates.is_empty());
        assert!(o.files);
    }

    #[test]
    fn value_arity_is_respected() {
        let r = deployer();
        // `--env prod` consumed its value, so the next word is positional
        let o = run(Some(&r), "deployer deploy --env prod ");
        assert!(o.candidates.is_empty());
        assert_eq!(o.slot.as_ref().unwrap().kind, SlotKind::Positional(0));
        // attached value does not consume the next word
        let o = run(Some(&r), "deployer deploy --env=prod --");
        assert!(values(&o).contains(&"--dry-run"));
        assert!(!values(&o).contains(&"--env"), "used non-repeatable flag hidden");
        // short cluster with value at the end: -je → -e's value next
        let o = run(Some(&r), "deployer deploy -e ");
        assert_eq!(values(&o).len(), 3);
    }

    #[test]
    fn repeatable_flags_stay_visible() {
        let r = deployer();
        let o = run(Some(&r), "deployer -v --verb");
        assert_eq!(values(&o), ["--verbose"]);
        // a used non-repeatable flag is hidden
        let o = run(Some(&r), "deployer --color auto --co");
        assert_eq!(values(&o), ["--config"]);
    }

    #[test]
    fn global_flag_value_before_subcommand() {
        let r = deployer();
        // `--config deploy` : deploy is the config value, not the subcommand
        let o = run(Some(&r), "deployer --config deploy ");
        assert_eq!(values(&o), ["deploy", "rollback", "status", "help"]);
        let o = run(Some(&r), "deployer --color always deploy --e");
        assert_eq!(values(&o), ["--env"]);
    }

    #[test]
    fn end_of_options_marker() {
        let r = deployer();
        let o = run(Some(&r), "deployer deploy -- --");
        assert!(o.candidates.is_empty(), "no flags after --");
        assert!(o.files);
        let o = run(Some(&r), "deployer -- ");
        assert!(!values(&o).contains(&"--config"));
    }

    #[test]
    fn expansions_are_not_guessed() {
        let r = deployer();
        let o = run(Some(&r), "deployer $SUB --");
        assert!(o.candidates.is_empty());
        let o = run(Some(&r), "deployer deploy --env $E");
        assert!(o.candidates.is_empty());
        assert!(o.files);
    }

    #[test]
    fn substitution_in_current_word_disables_file_fallback() {
        let r = deployer();
        let o = run(Some(&r), "deployer deploy --config=$(touch x)");
        assert!(o.candidates.is_empty());
        assert!(!o.files);
        // also when nothing is known about the command
        let o = run(None, "tool --x=`touch y`");
        assert!(!o.files);
        let o = run(Some(&r), "deployer deploy --config=$HOME/x");
        assert!(o.files, "plain variables keep the shell's file completion");
    }

    #[test]
    fn listed_subcommand_requests_discovery() {
        let r = deployer();
        let o = run(Some(&r), "deployer rollback --");
        assert!(o.candidates.is_empty());
        assert_eq!(o.discover, Some(vec!["rollback".to_string()]));
        // described nodes do not
        assert_eq!(run(Some(&r), "deployer deploy --").discover, None);
    }

    #[test]
    fn unicode_prefix_and_quoted_words() {
        let mut r = deployer();
        r.subcommands.push(CommandNode {
            name: "überprüfen".into(),
            status: NodeStatus::Listed,
            ..Default::default()
        });
        let o = run(Some(&r), "deployer üb");
        assert_eq!(values(&o), ["überprüfen"]);
        let o = run(Some(&r), "deployer 'dep");
        assert_eq!(values(&o), ["deploy"]);
    }

    #[test]
    fn deterministic_order() {
        let r = deployer();
        let a = run(Some(&r), "deployer deploy -");
        let b = run(Some(&r), "deployer deploy -");
        assert_eq!(a, b);
    }
}
