//! Shell-specific rendering of completion outcomes (protocol v1).
//!
//! The output is data, never shell code: Bash and Zsh receive NUL-framed
//! fields (NUL cannot occur in shell words), Fish receives the
//! `candidate<TAB>description` lines its `complete -a` contract expects, with
//! every field guaranteed free of tabs, newlines and control characters.
//!
//! Fish output: a header line `tabtrail1<TAB>FLAGS`, then one line per
//! candidate.
//!
//! Bash/Zsh frame layout (all fields NUL-terminated):
//!   bash: "tabtrail1" FLAGS COUNT REPLY*COUNT
//!   zsh:  "tabtrail1" FLAGS RAW_STRIP LOGICAL_STRIP COUNT (INSERT DISPLAY NOSPACE)*COUNT
//! FLAGS is a string of letters: `f` = file completion is appropriate,
//! `s` = do not append a space (bash, all candidates).

use crate::completion::{Candidate, Outcome};
use crate::context::CurrentWord;
use crate::sanitize::{display_text, is_safe_candidate};
use crate::shellword::Quote;

pub const PROTOCOL: &str = "tabtrail1";
const MAX_CANDIDATES: usize = 2000;

/// Raw byte offset in the input that corresponds to the logical character
/// index `logical` of the current word.
fn raw_offset(before: &str, cur: &CurrentWord, logical: usize) -> usize {
    cur.offsets.get(logical).copied().unwrap_or(before.len())
}

fn usable(c: &Candidate) -> bool {
    is_safe_candidate(&c.value)
}

fn flags(outcome: &Outcome, nospace: bool) -> String {
    let mut f = String::new();
    if outcome.files {
        f.push('f');
    }
    if nospace {
        f.push('s');
    }
    f
}

// ---------------------------------------------------------------- bash

fn bash_safe_char(c: char, first: bool) -> bool {
    if c.is_alphanumeric() && !c.is_ascii() {
        return true;
    }
    // `~` is always escaped: Bash also tilde-expands after `=`/`:` (`--dir=~/x`).
    c.is_ascii_alphanumeric()
        || matches!(c, '-' | '_' | '.' | ',' | '/' | ':' | '@' | '%' | '+' | '=' | '^')
        || (!first && c == '#')
}

/// Quotes `s` for insertion in the given quoting context of a Bash word.
/// Returns None when it cannot be represented safely.
pub fn bash_quote(s: &str, ctx: Option<Quote>, at_word_start: bool) -> Option<String> {
    let mut out = String::with_capacity(s.len() + 8);
    match ctx {
        None => {
            for (i, c) in s.chars().enumerate() {
                if !bash_safe_char(c, i == 0 && at_word_start) {
                    out.push('\\');
                }
                out.push(c);
            }
        }
        Some(Quote::Double) => {
            if s.contains('!') {
                return None; // history expansion cannot be escaped inside "..."
            }
            for c in s.chars() {
                if matches!(c, '"' | '\\' | '$' | '`') {
                    out.push('\\');
                }
                out.push(c);
            }
        }
        Some(Quote::Single) => {
            for c in s.chars() {
                if c == '\'' {
                    out.push_str("'\\''");
                } else {
                    out.push(c);
                }
            }
        }
        Some(Quote::AnsiC) => return None,
    }
    Some(out)
}

/// Start of the word Readline will replace: after an opening quote, or after
/// the last unescaped COMP_WORDBREAKS character. None when the word's quoting
/// is too complex to predict safely.
fn readline_word_start(before: &str, cur: &CurrentWord, wordbreaks: &str) -> Option<usize> {
    let raw = &before[cur.start..];
    if let Some((q, pos)) = cur.open_quote {
        if q == Quote::AnsiC {
            return None;
        }
        // Only a single opening quote, preceded by plain text, is supported.
        if before[cur.start..pos].contains(['\'', '"', '$', '`']) {
            return None;
        }
        return Some(pos + 1);
    }
    if raw.contains(['\'', '"']) {
        return None;
    }
    let mut start = cur.start;
    let mut escaped = false;
    for (i, c) in raw.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if wordbreaks.contains(c) && !c.is_whitespace() {
            start = cur.start + i + c.len_utf8();
        }
    }
    Some(start)
}

pub fn render_bash(before: &str, cur: &CurrentWord, outcome: &Outcome, wordbreaks: &str) -> Vec<u8> {
    let mut replies = Vec::new();
    let mut all_nospace = true;
    if let Some(word_start) = readline_word_start(before, cur, wordbreaks) {
        let repl_raw = raw_offset(before, cur, outcome.replace_from);
        let ctx = cur.open_quote.map(|(q, _)| q);
        let typed = &before[word_start.min(before.len())..];
        // Logical characters between the replacement point and the
        // Readline word start must be stripped from each candidate.
        let strip: String = cur
            .value
            .chars()
            .zip(cur.offsets.iter())
            .skip(outcome.replace_from)
            .filter(|&(_, &o)| o < word_start)
            .map(|(c, _)| c)
            .collect();
        let keep = if repl_raw > word_start {
            &before[word_start..repl_raw]
        } else {
            ""
        };
        for c in outcome.candidates.iter().filter(|c| usable(c)).take(MAX_CANDIDATES) {
            let Some(rest) = c.value.strip_prefix(strip.as_str()) else {
                continue;
            };
            let at_word_start = keep.is_empty() && strip.is_empty() && word_start == cur.start;
            let Some(quoted) = bash_quote(rest, ctx, at_word_start) else {
                continue;
            };
            let reply = format!("{keep}{quoted}");
            // Readline would otherwise replace typed text with a common
            // prefix that does not match what the user wrote.
            if !reply.starts_with(typed) {
                continue;
            }
            all_nospace &= c.no_space;
            replies.push(reply);
        }
    }
    let mut out = Vec::new();
    push_field(&mut out, PROTOCOL);
    push_field(&mut out, &flags(outcome, !replies.is_empty() && all_nospace));
    push_field(&mut out, &replies.len().to_string());
    for r in &replies {
        push_field(&mut out, r);
    }
    out
}

// ---------------------------------------------------------------- zsh

fn display_line(c: &Candidate) -> String {
    match &c.description {
        Some(d) if !d.is_empty() => format!("{}  -- {}", display_text(&c.value, 80), display_text(d, 120)),
        _ => display_text(&c.value, 80),
    }
}

pub fn render_zsh(before: &str, cur: &CurrentWord, outcome: &Outcome) -> Vec<u8> {
    let repl_raw = raw_offset(before, cur, outcome.replace_from);
    let raw_strip = before[cur.start.min(repl_raw)..repl_raw].chars().count();
    let logical_strip = outcome.replace_from;
    let cands: Vec<&Candidate> = outcome
        .candidates
        .iter()
        .filter(|c| usable(c))
        .take(MAX_CANDIDATES)
        .collect();
    let mut out = Vec::new();
    push_field(&mut out, PROTOCOL);
    push_field(&mut out, &flags(outcome, false));
    push_field(&mut out, &raw_strip.to_string());
    push_field(&mut out, &logical_strip.to_string());
    push_field(&mut out, &cands.len().to_string());
    for c in cands {
        push_field(&mut out, &c.value);
        push_field(&mut out, &display_line(c));
        push_field(&mut out, if c.no_space { "1" } else { "0" });
    }
    out
}

// ---------------------------------------------------------------- fish

pub fn render_fish(cur: &CurrentWord, outcome: &Outcome) -> Vec<u8> {
    // Fish replaces the whole token, so candidates carry the logical text
    // before the replacement point (e.g. "--env=").
    let head: String = cur.value.chars().take(outcome.replace_from).collect();
    let mut out = format!("{PROTOCOL}\t{}\n", flags(outcome, false));
    for c in outcome.candidates.iter().filter(|c| usable(c)).take(MAX_CANDIDATES) {
        let full = format!("{head}{}", c.value);
        if !is_safe_candidate(&full) {
            continue;
        }
        out.push_str(&full);
        if let Some(d) = c
            .description
            .as_deref()
            .map(|d| display_text(d, 120))
            .filter(|d| !d.is_empty())
        {
            out.push('\t');
            out.push_str(&d);
        }
        out.push('\n');
    }
    out.into_bytes()
}

fn push_field(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
    out.push(0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::CandidateKind;
    use crate::context::{Context, parse};
    use crate::shellword::Dialect;

    const WB: &str = " \t\n\"'><=;|&(:";

    fn cur(line: &str, d: Dialect) -> CurrentWord {
        match parse(line, d) {
            Context::Command(c) => c.current,
            other => panic!("{other:?}"),
        }
    }

    fn outcome(values: &[&str], replace_from: usize) -> Outcome {
        Outcome {
            candidates: values
                .iter()
                .map(|v| Candidate {
                    value: v.to_string(),
                    description: Some("desc\x1b[31m\tx".into()),
                    kind: CandidateKind::Value,
                    no_space: false,
                })
                .collect(),
            replace_from,
            files: false,
            ..Default::default()
        }
    }

    fn fields(bytes: &[u8]) -> Vec<String> {
        let mut v: Vec<String> = bytes
            .split(|b| *b == 0)
            .map(|s| String::from_utf8(s.to_vec()).unwrap())
            .collect();
        assert_eq!(v.pop().as_deref(), Some(""), "frame must end with NUL");
        v
    }

    fn bash(line: &str, values: &[&str], replace_from: usize) -> Vec<String> {
        let c = cur(line, Dialect::Bash);
        let f = fields(&render_bash(line, &c, &outcome(values, replace_from), WB));
        assert_eq!(f[0], PROTOCOL);
        let n: usize = f[2].parse().unwrap();
        assert_eq!(f.len(), 3 + n);
        f[3..].to_vec()
    }

    #[test]
    fn bash_plain_and_flag_value_after_wordbreak() {
        assert_eq!(bash("tool dep", &["deploy"], 0), ["deploy"]);
        // `=` is a COMP_WORDBREAKS char: Readline replaces only after it.
        assert_eq!(bash("tool --env=pr", &["prod"], 6), ["prod"]);
        // Candidates spanning a break char are trimmed to the Readline word.
        assert_eq!(bash("tool --opt:v", &["--opt:value"], 0), ["value"]);
    }

    #[test]
    fn bash_escapes_hostile_text() {
        assert_eq!(
            bash("tool x", &["x;rm -rf ~ $(id) `id`"], 0),
            [r"x\;rm\ -rf\ \~\ \$\(id\)\ \`id\`"]
        );
        assert_eq!(bash("tool ", &["~home", "#c", "a!b"], 0), [r"\~home", r"\#c", r"a\!b"]);
        // control characters cannot be inserted safely: dropped entirely
        assert!(bash("tool ", &["a\nb", "\x1b[2J"], 0).is_empty());
    }

    #[test]
    fn bash_open_quotes() {
        assert_eq!(bash("tool \"my f", &["my file$x"], 0), [r"my file\$x"]);
        assert!(bash("tool \"my f", &["my f!"], 0).is_empty());
        assert_eq!(bash("tool 'it", &["it's"], 0), [r"it'\''s"]);
        assert_eq!(bash("tool --env='pr", &["prod"], 6), ["prod"]);
        assert_eq!(bash("tool --env='pr", &["--env=prod"], 0), ["prod"]);
    }

    #[test]
    fn bash_backslash_typed_prefix_is_kept() {
        assert_eq!(bash(r"tool my\ f", &["my file"], 0), [r"my\ file"]);
        // typed text that cannot be a prefix of the quoted reply is skipped
        assert!(bash(r"tool m\y", &["my"], 0).is_empty());
    }

    #[test]
    fn bash_unicode() {
        assert_eq!(bash("tool üb", &["überprüfen"], 0), ["überprüfen"]);
        assert_eq!(bash("tool --mode=ü", &["über"], 7), ["über"]);
    }

    #[test]
    fn bash_complex_quoting_yields_nothing() {
        assert!(bash("tool a'b'c", &["abcd"], 0).is_empty());
        assert!(bash("tool $'a", &["ab"], 0).is_empty());
    }

    #[test]
    fn bash_flags_field() {
        let c = cur("tool ", Dialect::Bash);
        let mut o = outcome(&["a"], 0);
        o.files = true;
        o.candidates[0].no_space = true;
        assert_eq!(fields(&render_bash("tool ", &c, &o, WB))[1], "fs");
    }

    #[test]
    fn zsh_frames_and_strip_counts() {
        let line = "tool --env=pr";
        let c = cur(line, Dialect::Zsh);
        let f = fields(&render_zsh(line, &c, &outcome(&["prod", "x\ny"], 6)));
        assert_eq!(&f[..5], ["tabtrail1", "", "6", "6", "1"]);
        assert_eq!(f[5], "prod");
        assert_eq!(f[6], "prod  -- desc x");
        assert_eq!(f[7], "0");
        assert_eq!(f.len(), 8);
        // raw vs logical strip differ when the prefix contains quoting
        let line = r"tool --na\ me=v";
        let c = cur(line, Dialect::Zsh);
        let f = fields(&render_zsh(line, &c, &outcome(&["val"], 8)));
        assert_eq!(&f[2..4], ["9", "8"]);
    }

    #[test]
    fn fish_lines_include_token_head_and_safe_descriptions() {
        let c = cur("tool --env=pr", Dialect::Fish);
        let out = String::from_utf8(render_fish(&c, &outcome(&["prod", "bad\tvalue"], 6))).unwrap();
        assert_eq!(out, "tabtrail1\t\n--env=prod\tdesc x\n");
    }
}
