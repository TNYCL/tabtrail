//! Versioned, serializable command schema.
//!
//! A schema describes what is *known* about a command. Missing entries are
//! not proof that something does not exist: help output is often incomplete.

use serde::{Deserialize, Serialize};

/// Bumped whenever the serialized layout or its meaning changes.
pub const SCHEMA_VERSION: u32 = 2;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    /// Parsed from metadata describing this node itself.
    Described,
    /// Only known from the parent's listing; details not discovered yet.
    #[default]
    Listed,
    /// The command printed the parent's help for this node, so no separate
    /// metadata exists.
    NoSeparateHelp,
    /// Discovery was attempted and produced nothing usable.
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Output of running the (trusted) executable with a help switch.
    HelpOutput,
    /// A roff manual page read from disk; nothing was executed.
    ManPage,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub kind: SourceKind,
    /// The argv used for a help probe, or the manual page path.
    pub detail: String,
    pub parser_version: u32,
    /// Hash of the raw help output, to detect subcommands that only print
    /// their parent's help.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_hash: Option<u64>,
    /// For manual pages: identity (dev:ino:size:mtime_ns) of the file read,
    /// so an updated or removed page invalidates the cached metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_identity: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueKind {
    /// Unknown value type: the shell's file completion stays available.
    #[default]
    Any,
    Path,
    Directory,
    /// Numbers, durations, sizes: nothing sensible to complete.
    Number,
    /// A documented finite set (see `choices`).
    Choice,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValueSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub kind: ValueKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<Choice>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flag {
    /// All spellings including dashes, e.g. ["-e", "--env"].
    pub names: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<ValueSpec>,
    /// The value is optional and can only be attached (`--color[=WHEN]`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub value_optional: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub repeatable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl Flag {
    pub fn takes_separate_value(&self) -> bool {
        self.value.is_some() && !self.value_optional
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Positional {
    pub name: String,
    #[serde(default)]
    pub value: ValueSpec,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub variadic: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandNode {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub status: NodeStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<Flag>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subcommands: Vec<CommandNode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub positionals: Vec<Positional>,
    /// Usage text indicates that a subcommand is expected here.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub expects_subcommand: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Provenance>,
}

impl CommandNode {
    pub fn subcommand(&self, name: &str) -> Option<&CommandNode> {
        self.subcommands
            .iter()
            .find(|c| c.name == name || c.aliases.iter().any(|a| a == name))
    }

    pub fn subcommand_mut(&mut self, name: &str) -> Option<&mut CommandNode> {
        self.subcommands
            .iter_mut()
            .find(|c| c.name == name || c.aliases.iter().any(|a| a == name))
    }

    pub fn flag(&self, name: &str) -> Option<&Flag> {
        self.flags.iter().find(|f| f.names.iter().any(|n| n == name))
    }

    /// Follows `path` (subcommand names) from this node.
    pub fn descend(&self, path: &[String]) -> Option<&CommandNode> {
        let mut node = self;
        for p in path {
            node = node.subcommand(p)?;
        }
        Some(node)
    }

    pub fn descend_mut(&mut self, path: &[String]) -> Option<&mut CommandNode> {
        let mut node = self;
        for p in path {
            node = node.subcommand_mut(p)?;
        }
        Some(node)
    }

    /// True when this node carries any usable information.
    pub fn has_content(&self) -> bool {
        !self.flags.is_empty() || !self.subcommands.is_empty() || !self.positionals.is_empty()
    }
}

/// Identity of a resolved executable file, used to invalidate cached
/// metadata when the file is replaced or PATH resolves elsewhere.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutableIdentity {
    /// The resolved path (symlinks kept, so multi-call binaries keep argv[0]).
    pub path: String,
    /// Canonical path of the file actually executed.
    pub canonical: String,
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime_ns: i128,
}

/// Which sources were permitted when a document was collected. A document
/// collected under a narrower policy is re-collected when the policy widens
/// (e.g. after `tabtrail trust`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionPolicy {
    pub help: bool,
    pub man: bool,
    /// Fingerprint of the effective manual page search roots (derived from
    /// MANPATH, PATH and the executable's prefix); 0 when man pages are not
    /// used. Only this hash is stored, never the environment itself.
    #[serde(default)]
    pub man_roots: u64,
}

impl CollectionPolicy {
    /// True if `self` permits a source that `other` did not.
    pub fn wider_than(self, other: CollectionPolicy) -> bool {
        (self.help && !other.help) || (self.man && !other.man)
    }

    /// Identity of every manual page a node tree was built from.
    pub fn man_sources(node: &CommandNode, out: &mut Vec<(String, String)>) {
        if let Some(p) = &node.source
            && p.kind == SourceKind::ManPage
            && let Some(id) = &p.source_identity
        {
            out.push((p.detail.clone(), id.clone()));
        }
        for s in &node.subcommands {
            Self::man_sources(s, out);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaDocument {
    pub schema_version: u32,
    /// `help::PARSER_VERSION` at collection time.
    pub parser_version: u32,
    pub executable: ExecutableIdentity,
    /// Locale the metadata was collected under (probes force "C").
    pub locale: String,
    pub collected_at_unix: u64,
    #[serde(default)]
    pub collected_with: CollectionPolicy,
    pub root: CommandNode,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_tolerates_missing_optional_fields() {
        let doc = SchemaDocument {
            schema_version: SCHEMA_VERSION,
            parser_version: 1,
            executable: ExecutableIdentity {
                path: "/usr/bin/x".into(),
                canonical: "/usr/bin/x".into(),
                dev: 1,
                ino: 2,
                size: 3,
                mtime_ns: 4,
            },
            locale: "C".into(),
            collected_at_unix: 5,
            collected_with: CollectionPolicy {
                help: true,
                man: false,
                man_roots: 0,
            },
            root: CommandNode {
                name: "x".into(),
                status: NodeStatus::Described,
                flags: vec![Flag {
                    names: vec!["-e".into(), "--env".into()],
                    value: Some(ValueSpec {
                        placeholder: Some("ENV".into()),
                        kind: ValueKind::Choice,
                        choices: vec![Choice {
                            value: "prod".into(),
                            description: None,
                        }],
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            },
        };
        let json = serde_json::to_string(&doc).unwrap();
        let back: SchemaDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(back, doc);
        let minimal: CommandNode = serde_json::from_str(r#"{"name":"y"}"#).unwrap();
        assert_eq!(minimal.status, NodeStatus::Listed);
        assert!(minimal.flags.is_empty());
    }

    #[test]
    fn descend_follows_aliases() {
        let root = CommandNode {
            name: "root".into(),
            subcommands: vec![CommandNode {
                name: "remove".into(),
                aliases: vec!["rm".into()],
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(root.descend(&["rm".into()]).unwrap().name, "remove");
        assert!(root.descend(&["nope".into()]).is_none());
    }
}
