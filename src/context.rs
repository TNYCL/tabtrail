//! Interprets the tokenized text before the cursor: finds the simple command
//! the cursor belongs to and separates its command word, completed arguments
//! and the word being completed.
//!
//! Everything here is conservative. Constructs whose meaning depends on
//! evaluation (expansions in the command word, compound commands, wrappers
//! with options) produce `Context::NoCompletion`, which leaves the shell's own
//! completion in charge.

use crate::shellword::{Dialect, Op, Quote, Token, Word, tokenize};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArgWord {
    pub value: String,
    pub expansion: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentWord {
    /// Dequoted text of the word up to the cursor.
    pub value: String,
    /// Raw byte offset where the word starts (== input length for a new word).
    pub start: usize,
    /// Raw byte offset of each logical character.
    pub offsets: Vec<usize>,
    pub open_quote: Option<(Quote, usize)>,
    pub expansion: bool,
    /// Contains a command/process substitution.
    pub subst: bool,
    pub quoted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandContext {
    /// The command word as typed (dequoted).
    pub command: String,
    /// True when no wrapper/assignment/keyword preceded the command word, so
    /// the shell dispatched completion on this exact word.
    pub command_is_first_word: bool,
    pub args: Vec<ArgWord>,
    pub current: CurrentWord,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Context {
    Command(CommandContext),
    /// The cursor is on the command name itself.
    CommandName,
    /// The cursor is on a redirection target (a file name).
    RedirectTarget,
    NoCompletion(&'static str),
}

const POSIX_PREFIX_KEYWORDS: &[&str] = &["!", "if", "then", "else", "elif", "do", "while", "until", "{", "time"];
const POSIX_UNSUPPORTED_KEYWORDS: &[&str] = &[
    "case", "esac", "for", "select", "function", "[[", "]]", "((", "coproc", "in", "done", "fi", "}", "repeat",
    "foreach",
];
const FISH_PREFIX_KEYWORDS: &[&str] = &["and", "or", "not", "!", "if", "while", "else", "begin", "time"];
const FISH_UNSUPPORTED_KEYWORDS: &[&str] = &["for", "switch", "case", "function", "end", "return", "in"];
/// Commands that run their arguments as a command. Only accepted when used
/// without options, because their own options may take values.
const WRAPPERS: &[&str] = &["command", "exec", "nohup", "sudo", "doas", "env", "nice", "builtin"];

pub fn parse(before: &str, dialect: Dialect) -> Context {
    let tokens = match tokenize(before, dialect) {
        Ok(t) => t,
        Err(e) => return Context::NoCompletion(e.0),
    };
    let ends_in_word = tokens.ends_in_word;
    let mut toks = tokens.tokens;

    let current = if ends_in_word {
        match toks.pop() {
            Some(Token::Word(w)) => w,
            _ => return Context::NoCompletion("internal: expected word"),
        }
    } else {
        Word {
            start: before.len(),
            end: before.len(),
            value: String::new(),
            offsets: Vec::new(),
            expansion: false,
            subst: false,
            quoted: false,
            open_quote: None,
        }
    };

    // Collect the words of the last simple command, dropping redirections.
    let mut segment: Vec<Word> = Vec::new();
    let mut redirect_pending = false;
    for t in toks {
        match t {
            Token::Op { op: Op::Redirect, .. } => redirect_pending = true,
            Token::Op { .. } => {
                segment.clear();
                redirect_pending = false;
            }
            Token::Word(w) => {
                if redirect_pending {
                    redirect_pending = false;
                } else {
                    segment.push(w);
                }
            }
        }
    }
    if redirect_pending {
        return Context::RedirectTarget;
    }

    let mut idx = 0;
    let mut first = true;
    let is_plain = |w: &Word| !w.quoted && !w.expansion;
    loop {
        let Some(w) = segment.get(idx) else {
            // No command word yet: the cursor is on the command position,
            // unless the current word is itself an assignment value.
            return if is_assignment(&current) {
                Context::NoCompletion("assignment value")
            } else {
                Context::CommandName
            };
        };
        if is_assignment(w) {
            idx += 1;
            first = false;
            continue;
        }
        if is_plain(w) {
            let v = w.value.as_str();
            let (prefix_kw, unsupported_kw) = if dialect.eq(&Dialect::Fish) {
                (FISH_PREFIX_KEYWORDS, FISH_UNSUPPORTED_KEYWORDS)
            } else {
                (POSIX_PREFIX_KEYWORDS, POSIX_UNSUPPORTED_KEYWORDS)
            };
            if unsupported_kw.contains(&v) {
                return Context::NoCompletion("compound command syntax");
            }
            if prefix_kw.contains(&v) || (dialect == Dialect::Fish && matches!(v, "command" | "builtin" | "exec")) {
                idx += 1;
                first = false;
                if v == "time" && segment.get(idx).is_some_and(|n| n.value == "-p") {
                    idx += 1;
                }
                continue;
            }
            if WRAPPERS.contains(&v) {
                if v == "builtin" {
                    return Context::NoCompletion("shell builtin");
                }
                // The wrapper's own options may take values; be conservative.
                let next_is_option = segment.get(idx + 1).map(|n| n.value.starts_with('-')).unwrap_or(
                    // the current word being an option of the wrapper
                    current.value.starts_with('-') && idx + 1 == segment.len(),
                );
                if next_is_option {
                    return Context::NoCompletion("wrapper with options");
                }
                idx += 1;
                first = false;
                continue;
            }
        }
        break;
    }

    let cmd = &segment[idx];
    if cmd.expansion || cmd.value.is_empty() {
        return Context::NoCompletion("command word needs expansion");
    }
    let args = segment[idx + 1..]
        .iter()
        .map(|w| ArgWord {
            value: w.value.clone(),
            expansion: w.expansion,
        })
        .collect();
    Context::Command(CommandContext {
        command: cmd.value.clone(),
        command_is_first_word: first,
        args,
        current: CurrentWord {
            value: current.value,
            start: current.start,
            offsets: current.offsets,
            open_quote: current.open_quote,
            expansion: current.expansion,
            subst: current.subst,
            quoted: current.quoted,
        },
    })
}

/// `NAME=value` with an unquoted, literal name.
fn is_assignment(w: &Word) -> bool {
    let Some(eq) = w.value.find('=') else {
        return false;
    };
    let name = &w.value[..eq];
    let mut chars = name.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    // The name and '=' must be literal characters (not quoted or expanded).
    valid && w.offsets.get(eq).is_some_and(|&o| o == w.start + eq)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(input: &str, d: Dialect) -> CommandContext {
        match parse(input, d) {
            Context::Command(c) => c,
            other => panic!("{input:?}: expected command context, got {other:?}"),
        }
    }

    fn arg_values(c: &CommandContext) -> Vec<&str> {
        c.args.iter().map(|a| a.value.as_str()).collect()
    }

    #[test]
    fn simple_command() {
        let c = cmd("tool deploy --env pr", Dialect::Bash);
        assert_eq!(c.command, "tool");
        assert!(c.command_is_first_word);
        assert_eq!(arg_values(&c), ["deploy", "--env"]);
        assert_eq!(c.current.value, "pr");
        assert_eq!(c.current.start, "tool deploy --env ".len());
    }

    #[test]
    fn new_empty_word_after_space() {
        let c = cmd("tool deploy ", Dialect::Bash);
        assert_eq!(arg_values(&c), ["deploy"]);
        assert_eq!(c.current.value, "");
        assert_eq!(c.current.start, "tool deploy ".len());
    }

    #[test]
    fn last_pipeline_segment_is_used() {
        let c = cmd("cat x | grep -i foo && tool sub ar", Dialect::Bash);
        assert_eq!(c.command, "tool");
        assert_eq!(arg_values(&c), ["sub"]);
        assert_eq!(c.current.value, "ar");
    }

    #[test]
    fn redirections_are_removed_and_targets_detected() {
        let c = cmd("tool 2>/dev/null sub >out ar", Dialect::Bash);
        assert_eq!(arg_values(&c), ["sub"]);
        assert_eq!(parse("tool sub > ou", Dialect::Bash), Context::RedirectTarget);
        assert_eq!(parse("tool sub > ", Dialect::Bash), Context::RedirectTarget);
    }

    #[test]
    fn assignments_keywords_and_wrappers_are_skipped() {
        let c = cmd("FOO=1 BAR='x y' sudo env A=b nohup tool s", Dialect::Bash);
        assert_eq!(c.command, "tool");
        assert!(!c.command_is_first_word);
        assert_eq!(c.current.value, "s");
        let c = cmd("if tool --fl", Dialect::Bash);
        assert_eq!(c.command, "tool");
        let c = cmd("not command tool --fl", Dialect::Fish);
        assert_eq!(c.command, "tool");
    }

    #[test]
    fn wrapper_with_options_is_not_guessed() {
        assert!(matches!(
            parse("sudo -u root tool --f", Dialect::Bash),
            Context::NoCompletion(_)
        ));
        assert!(matches!(parse("env -i tool ", Dialect::Bash), Context::NoCompletion(_)));
    }

    #[test]
    fn command_position() {
        assert_eq!(parse("", Dialect::Bash), Context::CommandName);
        assert_eq!(parse("too", Dialect::Bash), Context::CommandName);
        assert_eq!(parse("ls | gr", Dialect::Bash), Context::CommandName);
        assert_eq!(parse("FOO=1 to", Dialect::Bash), Context::CommandName);
        assert!(matches!(parse("FOO=ba", Dialect::Bash), Context::NoCompletion(_)));
    }

    #[test]
    fn expanded_command_word_is_not_resolved() {
        for input in ["$EDITOR --f", "$(which tool) --f", "~/bin/tool --f", "t*l --f"] {
            assert!(
                matches!(parse(input, Dialect::Bash), Context::NoCompletion(_)),
                "{input}"
            );
        }
        // quoting without expansion is fine
        assert_eq!(cmd("'my tool' --f", Dialect::Bash).command, "my tool");
    }

    #[test]
    fn compound_syntax_falls_back() {
        for input in ["for x in a b; do tool", "case $x in a) tool --f"] {
            // `do tool` after `for` would be a separate segment; the first
            // segment of a for loop must not be guessed.
            let _ = parse(input, Dialect::Bash);
        }
        assert!(matches!(parse("for x in a", Dialect::Bash), Context::NoCompletion(_)));
        assert!(matches!(parse("[[ -f x", Dialect::Bash), Context::NoCompletion(_)));
        assert!(matches!(parse("switch $x", Dialect::Fish), Context::NoCompletion(_)));
    }

    #[test]
    fn substitution_inside_line_is_never_run_and_cursor_inside_falls_back() {
        let c = cmd("tool --name $(rm -rf ~) sub ", Dialect::Bash);
        assert_eq!(c.args.len(), 3);
        assert!(c.args[1].expansion);
        assert!(matches!(
            parse("tool $(rm -rf ~", Dialect::Bash),
            Context::NoCompletion(_)
        ));
    }

    #[test]
    fn subshell_and_grouping() {
        assert_eq!(cmd("(cd x && tool --f", Dialect::Bash).command, "tool");
        assert_eq!(cmd("{ tool --f", Dialect::Bash).command, "tool");
    }

    #[test]
    fn quoted_name_is_not_an_assignment() {
        // "FOO"=1 is a command word in bash, not an assignment
        let c = cmd("'FOO=1' x", Dialect::Bash);
        assert_eq!(c.command, "FOO=1");
    }
}
