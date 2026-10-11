//! Heuristic parser for `--help` style text (and man pages rendered to
//! text by `man.rs`).
//!
//! This is deliberately strict: a line becomes a flag only when it has the
//! layout of a help table row (flag spec followed by two spaces or the end of
//! the line), and a subcommand only inside a section whose header names
//! commands. Prose that merely mentions `--something` is ignored, so the
//! parser returns less rather than inventing options.

use crate::sanitize::{display_text, strip_escapes};
use crate::schema::{Choice, CommandNode, Flag, NodeStatus, Positional, ValueKind, ValueSpec};

/// Bumped whenever parsing behaviour changes, invalidating cached schemas.
pub const PARSER_VERSION: u32 = 5;

const MAX_DESCRIPTION: usize = 160;
const MAX_LINES: usize = 20_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Section {
    None,
    Usage,
    Commands,
    Options,
    Arguments,
    Examples,
    Other,
}

fn classify_header(title: &str) -> Section {
    let t = title.to_ascii_lowercase();
    if t.starts_with("usage") || t == "synopsis" {
        Section::Usage
    } else if t.contains("example") {
        Section::Examples
    } else if t.contains("command") && !t.contains("option") {
        Section::Commands
    } else if t.contains("option") || t.contains("flag") || t.contains("switch") {
        Section::Options
    } else if t.contains("argument") || t.contains("positional") {
        Section::Arguments
    } else {
        Section::Other
    }
}

/// Recognizes a section header line, returning its title.
fn header_title(line: &str) -> Option<&str> {
    let indent = indentation(line);
    let t = line.trim();
    if t.is_empty() || indent > 2 || t.starts_with('-') {
        return None;
    }
    // Long sentence-like headers are accepted only when they announce
    // commands ("These are common X commands used in ...:").
    if t.len() > 48 && !(t.len() <= 100 && t.ends_with(':') && t.to_ascii_lowercase().contains("command")) {
        return None;
    }
    if let Some(title) = t.strip_suffix(':') {
        if !title.contains("  ") && !title.is_empty() && !title.ends_with(':') && !title.contains("://") {
            return Some(title);
        }
        return None;
    }
    // Man-style all-caps headers: "OPTIONS", "GLOBAL OPTIONS".
    if indent == 0
        && t.chars().any(|c| c.is_ascii_uppercase())
        && t.chars().all(|c| c.is_ascii_uppercase() || c == ' ' || c == '-')
    {
        return Some(t);
    }
    None
}

fn indentation(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ').count()
}

/// Expands tabs to 8-column stops. A tab after text separates columns, so
/// it always yields at least two spaces (the column gap the parser expects).
fn expand_tabs(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut col = 0;
    for c in line.chars() {
        if c == '\t' {
            let mut n = 8 - (col % 8);
            if out.chars().any(|c| c != ' ') && n < 2 {
                n += 8;
            }
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else {
            out.push(c);
            col += 1;
        }
    }
    out
}

const BOX_HORIZONTAL: &[char] = &['─', '━', '═'];

/// Unwraps box-drawn panels (rich/Typer/rich-click style):
/// `╭─ Options ─╮` becomes the header `Options:`, `│ row │` becomes the row,
/// and the bottom border becomes a blank line.
fn unbox(line: &str) -> String {
    let t = line.trim();
    let first = t.chars().next();
    match first {
        Some('╭' | '┌' | '┏' | '╔') => {
            let title = t.trim_matches(|c: char| {
                matches!(c, '╭' | '╮' | '┌' | '┐' | '┏' | '┓' | '╔' | '╗' | ' ') || BOX_HORIZONTAL.contains(&c)
            });
            if title.is_empty() {
                String::new()
            } else {
                format!("{title}:")
            }
        }
        Some('╰' | '└' | '┗' | '╚') => String::new(),
        Some('│' | '┃' | '║') => {
            let inner = &t[first.map_or(0, char::len_utf8)..];
            let inner = inner.trim_end().trim_end_matches(['│', '┃', '║']);
            format!("  {}", inner.trim_end())
        }
        _ => line.to_string(),
    }
}

fn preprocess(text: &str) -> Vec<String> {
    strip_escapes(text)
        .lines()
        .take(MAX_LINES)
        .map(|l| {
            let l: String = l.chars().filter(|c| *c == '\t' || !c.is_control()).collect();
            unbox(expand_tabs(&l).trim_end())
        })
        .collect()
}

fn valid_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '+' | '@')
}

/// A subcommand or choice name that is safe to offer.
/// Letters/digits of any script are accepted; shell metacharacters,
/// whitespace and control characters never are.
pub fn is_valid_word(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_alphanumeric())
        && chars.all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '+' | '/' | '@'))
        && s.chars().count() <= 64
}

const TYPE_WORDS: &[&str] = &[
    "string",
    "strings",
    "int",
    "int32",
    "int64",
    "uint",
    "uint32",
    "uint64",
    "float",
    "float32",
    "float64",
    "duration",
    "value",
    "stringArray",
    "stringSlice",
    "intSlice",
    "file",
    "path",
    "dir",
    "text",
    "bytes",
    "list",
    "num",
    "number",
    "count",
];

#[derive(Debug, Default)]
struct FlagLine {
    names: Vec<String>,
    placeholder: Option<String>,
    value_optional: bool,
    repeatable: bool,
    description: String,
}

/// Parses one flag spec name at `s`, returning (names, bytes consumed).
fn parse_flag_name(s: &str) -> Option<(Vec<String>, usize)> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'-') {
        return None;
    }
    let dashes = if bytes.get(1) == Some(&b'-') { 2 } else { 1 };
    let mut i = dashes;
    let mut negatable = false;
    let mut gflags_negatable = false;
    if dashes == 2 && s[i..].starts_with("[no-]") {
        negatable = true;
        i += 5;
    } else if dashes == 2 && s[i..].starts_with("[no]") {
        // gflags/Abseil boolean: --[no]name
        gflags_negatable = true;
        i += 4;
    }
    let rest = &s[i..];
    let first = rest.chars().next()?;
    let single_short_ok = dashes == 1 && matches!(first, '?' | '#' | '@');
    if !first.is_ascii_alphanumeric() && !single_short_ok {
        return None;
    }
    let len: usize = rest
        .char_indices()
        .take_while(|&(idx, c)| if idx == 0 { true } else { valid_name_char(c) })
        .map(|(_, c)| c.len_utf8())
        .sum();
    let mut name_body = &rest[..len];
    // A trailing '.' or ':' belongs to prose, not the name.
    while name_body.len() > 1 && (name_body.ends_with('.') || name_body.ends_with(':')) {
        name_body = &name_body[..name_body.len() - 1];
    }
    let consumed = i + name_body.len();
    let prefix = &s[..dashes];
    let mut names = vec![format!("{prefix}{name_body}")];
    if negatable {
        names.push(format!("--no-{name_body}"));
    }
    if gflags_negatable {
        names.push(format!("--no{name_body}"));
    }
    Some((names, consumed))
}

fn take_token(s: &str) -> &str {
    let end = s.find([' ', ',']).unwrap_or(s.len());
    &s[..end]
}

/// Recognizes a value placeholder token following a flag name.
fn is_placeholder(tok: &str) -> bool {
    if tok.is_empty() {
        return false;
    }
    let bracketed = (tok.starts_with('<') && tok.contains('>'))
        || (tok.starts_with('{') && tok.contains('}'))
        || (tok.starts_with('[') && tok.contains(']') && !tok.starts_with("[-"));
    if bracketed {
        return true;
    }
    let base = tok.trim_end_matches("...");
    let upper = base.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && base
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' || c == '-');
    upper || TYPE_WORDS.contains(&base)
}

fn parse_flag_line(trimmed: &str) -> Option<FlagLine> {
    let mut fl = FlagLine::default();
    let mut s = trimmed;
    loop {
        let (names, used) = parse_flag_name(s)?;
        fl.names.extend(names);
        s = &s[used..];
        if let Some(r) = s.strip_prefix("...") {
            fl.repeatable = true;
            s = r;
        }
        // value part
        if let Some(r) = s.strip_prefix("[=") {
            let end = r.find(']')?;
            fl.placeholder = Some(r[..end].to_string());
            fl.value_optional = true;
            s = &r[end + 1..];
        } else if let Some(r) = s.strip_prefix('=') {
            let tok = take_token(r);
            if tok.is_empty() {
                return None;
            }
            fl.placeholder = Some(tok.to_string());
            s = &r[tok.len()..];
        } else if let Some(r) = s.strip_prefix(' ') {
            // `(` starts a gflags description, handled after the loop.
            if !r.starts_with(' ')
                && !r.starts_with('-')
                && !r.starts_with('/')
                && !r.starts_with('|')
                && !r.starts_with('(')
            {
                let tok = if r.starts_with('<') {
                    r.find('>').map(|e| &r[..=e]).unwrap_or("")
                } else if r.starts_with('{') {
                    r.find('}').map(|e| &r[..=e]).unwrap_or("")
                } else if r.starts_with('[') {
                    r.find(']').map(|e| &r[..=e]).unwrap_or("")
                } else {
                    take_token(r)
                };
                if is_placeholder(tok) {
                    fl.placeholder = Some(tok.to_string());
                    s = &r[tok.len()..];
                } else {
                    // A single space followed by prose: not a help table row.
                    return None;
                }
            }
        }
        if let Some(r) = s.strip_prefix("...") {
            fl.repeatable = true;
            s = r;
        }
        // alias separators
        let next = [", ", ",", " | ", "|", " / ", " "]
            .iter()
            .find_map(|sep| s.strip_prefix(sep).filter(|r| r.starts_with('-')));
        match next {
            Some(r) => s = r,
            None => break,
        }
    }
    if s.is_empty() {
        return Some(fl);
    }
    // Swift ArgumentParser: when the spec fills its column, a single space
    // follows a bracketed placeholder; the bracket delimits it unambiguously.
    if !s.starts_with("  ") && s.starts_with(' ') && fl.placeholder.as_deref().is_some_and(|p| p.starts_with('<')) {
        fl.description = s.trim().to_string();
        return Some(fl);
    }
    // gflags/Abseil: `--name (description); default: VALUE;`
    if let Some(r) = s.strip_prefix(" (") {
        if fl.placeholder.is_some() {
            return None;
        }
        let close = r.rfind(");")?;
        fl.description = r[..close].trim().to_string();
        let after = r[close + 2..].trim();
        let default = after.strip_prefix("default:")?.trim().trim_end_matches(';').trim();
        if !matches!(default, "true" | "false") {
            fl.placeholder = Some("value".to_string());
        }
        return Some(fl);
    }
    if s.starts_with("  ") {
        let mut rest = s.trim_start();
        // Column layouts (rich/Typer): an alias column and/or a metavar
        // column may precede the description, each followed by 2+ spaces.
        while rest.starts_with('-') {
            let Some((names, used)) = parse_flag_name(rest) else {
                break;
            };
            let after = &rest[used..];
            if !(after.is_empty() || after.starts_with("  ")) {
                break;
            }
            fl.names.extend(names);
            rest = after.trim_start();
        }
        if fl.placeholder.is_none() {
            let close = match rest.chars().next() {
                Some('[') => rest.find(']'),
                Some('<') => rest.find('>'),
                Some('{') => rest.find('}'),
                _ => None,
            };
            if let Some(end) = close {
                let tok = &rest[..=end];
                let after = &rest[end + 1..];
                let is_choice_or_metavar = tok.starts_with('<') || tok.contains('|') || tok.contains(',');
                if is_choice_or_metavar && (after.is_empty() || after.starts_with("  ")) {
                    fl.placeholder = Some(tok.to_string());
                    rest = after.trim_start();
                }
            }
        }
        fl.description = rest.trim().to_string();
        return Some(fl);
    }
    None
}

fn strip_quotes(s: &str) -> &str {
    s.trim()
        .trim_matches(|c| matches!(c, '"' | '\'' | '`' | '(' | ')' | '.' | ';'))
}

/// Parses a list such as `a, b, c`, `a|b|c` or `"a", "b"`. Returns None
/// unless every item looks like a value token and there are at least two.
fn parse_choice_list(s: &str) -> Option<Vec<String>> {
    let s = s.trim();
    let sep = if s.contains('|') { '|' } else { ',' };
    let mut out = Vec::new();
    for item in s.split(sep) {
        let mut item = strip_quotes(item);
        if let Some(r) = item.strip_prefix("or ") {
            item = strip_quotes(r);
        }
        if item.is_empty() {
            continue;
        }
        if !is_valid_word(item) {
            return None;
        }
        out.push(item.to_string());
    }
    (out.len() >= 2).then_some(out)
}

/// Extracts documented finite values from a placeholder or description.
fn extract_choices(placeholder: Option<&str>, description: &str) -> Vec<String> {
    if let Some(p) = placeholder {
        let inner = p
            .trim_end_matches("...")
            .trim_start_matches(['<', '[', '{'])
            .trim_end_matches(['>', ']', '}']);
        if (inner.contains('|') || (p.starts_with('{') && inner.contains(',')))
            && let Some(list) = parse_choice_list(inner)
        {
            return list;
        }
    }
    let lower = description.to_ascii_lowercase();
    for key in [
        "possible values:",
        "choices:",
        "one of:",
        "one of",
        "allowed values:",
        "valid values:",
        "values:",
    ] {
        if let Some(pos) = lower.find(key) {
            let after = &description[pos + key.len()..];
            // Stop at a closing bracket or at a following "default".
            let mut end = after.find([']', ')']).unwrap_or(after.len());
            if after.trim_start().starts_with('(') {
                let open = after.find('(').unwrap_or(0);
                end = after[open..].find(')').map(|e| open + e).unwrap_or(after.len());
            }
            let mut segment = &after[..end];
            if let Some(d) = segment.to_ascii_lowercase().find("default") {
                segment = &segment[..d];
            }
            let segment = segment.trim().trim_start_matches('(');
            if let Some(list) = parse_choice_list(segment) {
                return list;
            }
        }
    }
    Vec::new()
}

fn value_kind_for(placeholder: &str) -> ValueKind {
    let p = placeholder
        .trim_end_matches("...")
        .trim_matches(|c| matches!(c, '<' | '>' | '[' | ']' | '{' | '}' | '='))
        .to_ascii_uppercase();
    if p.contains("DIR") || p.contains("FOLDER") {
        ValueKind::Directory
    } else if p.contains("FILE") || p.contains("PATH") {
        ValueKind::Path
    } else if matches!(
        p.as_str(),
        "N" | "NUM"
            | "NUMBER"
            | "INT"
            | "INT32"
            | "INT64"
            | "UINT"
            | "UINT32"
            | "UINT64"
            | "FLOAT"
            | "FLOAT32"
            | "FLOAT64"
            | "COUNT"
            | "SECONDS"
            | "SECS"
            | "MS"
            | "DURATION"
            | "SIZE"
            | "BYTES"
            | "PORT"
            | "LIMIT"
            | "WIDTH"
            | "DEPTH"
            | "TIMEOUT"
    ) {
        ValueKind::Number
    } else {
        ValueKind::Any
    }
}

/// yargs-style trailing type tags: `[string]`, `[number]`, `[array]`, or a
/// `[choices: ...]` list mean the option takes a value even though no
/// placeholder is printed.
fn tagged_value_type(description: &str) -> Option<&'static str> {
    let d = description.to_ascii_lowercase();
    for (tag, placeholder) in [
        ("[string]", "string"),
        ("[number]", "number"),
        ("[array]", "array"),
        ("[choices:", "value"),
    ] {
        if d.contains(tag) {
            return Some(placeholder);
        }
    }
    None
}

fn build_flag(mut fl: FlagLine, extra_choices: Vec<Choice>) -> Flag {
    if fl.placeholder.is_none()
        && let Some(p) = tagged_value_type(&fl.description)
    {
        fl.placeholder = Some(p.to_string());
    }
    let description = (!fl.description.is_empty()).then(|| display_text(&fl.description, MAX_DESCRIPTION));
    let value = fl.placeholder.as_deref().map(|p| {
        let mut choices: Vec<Choice> = extract_choices(Some(p), &fl.description)
            .into_iter()
            .map(|value| Choice {
                value,
                description: None,
            })
            .collect();
        if choices.is_empty() {
            choices = extra_choices;
        }
        let kind = if choices.is_empty() {
            value_kind_for(p)
        } else {
            ValueKind::Choice
        };
        ValueSpec {
            placeholder: Some(display_text(p, 40)),
            kind,
            choices,
        }
    });
    Flag {
        names: fl.names,
        value,
        value_optional: fl.value_optional,
        repeatable: fl.repeatable,
        description,
    }
}

#[derive(Debug)]
struct SubLine {
    names: Vec<String>,
    description: String,
}

fn parse_subcommand_line(trimmed: &str) -> Option<SubLine> {
    let name_end = trimmed
        .find(|c: char| !(c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '|' | ',')))
        .unwrap_or(trimmed.len());
    let name_part = trimmed[..name_end].trim_end_matches(',');
    let mut names = Vec::new();
    for n in name_part.split(['|', ',']) {
        let n = n.trim();
        if n.is_empty() {
            continue;
        }
        // Trailing punctuation marks prose ("output.", "Interrogators:").
        if !is_valid_word(n) || n.starts_with('-') || n.ends_with(['.', ':']) {
            return None;
        }
        names.push(n.to_string());
    }
    if names.is_empty() {
        return None;
    }
    let mut rest = &trimmed[name_end..];
    // ", alias" lists with a space after the comma
    while let Some(r) = rest.strip_prefix(", ") {
        let tok = take_token(r);
        if !is_valid_word(tok) {
            return None;
        }
        names.push(tok.to_string());
        rest = &r[tok.len()..];
    }
    // commander-style argument spec: name [options] <arg>
    loop {
        if rest.is_empty() || rest.starts_with("  ") {
            break;
        }
        let r = rest.strip_prefix(' ')?;
        if r.starts_with('[') || r.starts_with('<') {
            let close = if r.starts_with('[') { ']' } else { '>' };
            let end = r.find(close)?;
            let mut after = &r[end + 1..];
            if let Some(a) = after.strip_prefix("...") {
                after = a;
            }
            rest = after;
        } else {
            return None;
        }
    }
    Some(SubLine {
        names,
        description: rest.trim().to_string(),
    })
}

#[derive(Debug, Default)]
struct UsageInfo {
    forms: Vec<String>,
}

/// Literal words directly after the program name in usage forms (docopt
/// style: `prog ship new <name>`, `prog ship <name> move`). Accepted only as
/// a repeated pattern: one literal in at least two forms, or at least two
/// distinct literals, so a single `prog source dest` is not misread.
fn usage_literal_subcommands(usage: &UsageInfo, command_path: &[&str]) -> Vec<String> {
    let mut seen: Vec<(String, usize)> = Vec::new();
    for form in &usage.forms {
        let mut toks = form.split_whitespace();
        if toks.next().is_none() {
            continue;
        }
        let mut toks = toks.skip_while(|t| command_path.contains(t));
        let Some(t) = toks.next() else { continue };
        let literal =
            t.chars().next().is_some_and(|c| c.is_ascii_lowercase()) && is_valid_word(t) && !TYPE_WORDS.contains(&t);
        if literal {
            match seen.iter_mut().find(|(w, _)| w == t) {
                Some((_, n)) => *n += 1,
                None => seen.push((t.to_string(), 1)),
            }
        }
    }
    let repeated = seen.iter().any(|(_, n)| *n >= 2) || seen.len() >= 2;
    if repeated {
        seen.into_iter().map(|(w, _)| w).collect()
    } else {
        Vec::new()
    }
}

/// Top-level bracket groups of a usage form, e.g. "[-C <path>]".
fn bracket_groups(form: &str) -> Vec<&str> {
    let mut groups = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, c) in form.char_indices() {
        match c {
            '[' => {
                if depth == 0 {
                    start = i + 1;
                }
                depth += 1;
            }
            ']' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    groups.push(&form[start..i]);
                }
            }
            _ => {}
        }
    }
    groups
}

/// Options documented only in usage lines: `[-C <path>]`,
/// `[-v | --version]`, `[--exec-path[=<path>]]`, `[-46Aq]` (a cluster of
/// single-letter switches). Only groups that consist entirely of option
/// syntax are used.
fn usage_flags(usage: &UsageInfo) -> Vec<Flag> {
    let mut flags: Vec<Flag> = Vec::new();
    for form in &usage.forms {
        for group in bracket_groups(form) {
            let group = group.trim();
            if !group.starts_with('-') || group.contains('[') && !group.contains("[=") {
                continue;
            }
            let mut flag = Flag::default();
            let mut ok = true;
            for alt in group.split(" | ").map(str::trim) {
                let toks: Vec<&str> = alt.split_whitespace().collect();
                let Some(first) = toks.first() else {
                    ok = false;
                    break;
                };
                let is_cluster = toks.len() == 1
                    && !first.starts_with("--")
                    && first.len() > 2
                    && first[1..].chars().all(|c| c.is_ascii_alphanumeric());
                if is_cluster {
                    for c in first[1..].chars() {
                        flags.push(Flag {
                            names: vec![format!("-{c}")],
                            ..Default::default()
                        });
                    }
                    continue;
                }
                // Inside brackets there is no prose: `-p port` is a flag and
                // its value even with a lowercase placeholder.
                let fl = if toks.len() == 2 && !toks[1].starts_with('-') {
                    match parse_flag_name(toks[0]) {
                        Some((names, used)) if used == toks[0].len() => FlagLine {
                            names,
                            placeholder: Some(toks[1].to_string()),
                            ..Default::default()
                        },
                        _ => {
                            ok = false;
                            break;
                        }
                    }
                } else {
                    match parse_flag_line(alt) {
                        Some(fl) if fl.description.is_empty() && toks.len() <= 2 => fl,
                        _ => {
                            ok = false;
                            break;
                        }
                    }
                };
                flag.names.extend(fl.names);
                if let Some(p) = fl.placeholder {
                    flag.value = Some(ValueSpec {
                        kind: value_kind_for(&p),
                        placeholder: Some(display_text(&p, 40)),
                        choices: Vec::new(),
                    });
                    flag.value_optional = fl.value_optional;
                }
            }
            if ok && !flag.names.is_empty() {
                flags.push(flag);
            }
        }
    }
    flags
}

fn parse_usage_forms(usage: &UsageInfo, command_path: &[&str]) -> (bool, Vec<Positional>) {
    let mut expects_sub = false;
    let mut first_positionals: Option<Vec<Positional>> = None;
    for form in &usage.forms {
        let mut toks = form.split_whitespace().peekable();
        // Skip the program name (possibly a path) and subcommand path words.
        if toks.next().is_none() {
            continue;
        }
        while toks.peek().is_some_and(|t| command_path.contains(t)) {
            toks.next();
        }
        let mut positionals = Vec::new();
        let mut depth: i32 = 0;
        let mut stop = false;
        let mut skip_value = false;
        for tok in toks {
            if stop {
                break;
            }
            if std::mem::take(&mut skip_value) && depth == 0 && is_placeholder(tok) {
                continue; // the value of a preceding `--flag VALUE`
            }
            let opens = tok.matches(['[', '{', '(']).count() as i32;
            let closes = tok.matches([']', '}', ')']).count() as i32;
            let started_at = depth;
            depth = (depth + opens - closes).max(0);
            if started_at > 0 {
                continue; // inside a multi-token bracket such as [-o OUTPUT]
            }
            let bare = tok
                .trim_end_matches("...")
                .trim_matches(|c| matches!(c, '[' | ']' | '<' | '>'));
            let lower = bare.to_ascii_lowercase();
            if tok.starts_with('-') {
                skip_value = !tok.contains('=') && tok != "--";
                continue;
            }
            if tok.starts_with("[-") || lower.starts_with("option") || lower == "flags" || tok == "..." {
                continue;
            }
            if matches!(lower.as_str(), "command" | "subcommand" | "cmd" | "commands")
                || (tok.starts_with('{') && tok.contains(','))
            {
                expects_sub = true;
                stop = true;
                continue;
            }
            if lower == "args" || lower == "arguments" || lower == "arg" {
                continue;
            }
            if bare.is_empty()
                || !bare
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
            {
                continue;
            }
            positionals.push(Positional {
                name: bare.to_string(),
                value: ValueSpec {
                    placeholder: Some(bare.to_string()),
                    kind: value_kind_for(bare),
                    choices: Vec::new(),
                },
                variadic: tok.ends_with("...") || tok.ends_with("]..."),
                optional: tok.starts_with('['),
            });
        }
        if first_positionals.is_none() && !positionals.is_empty() {
            first_positionals = Some(positionals);
        }
    }
    (expects_sub, first_positionals.unwrap_or_default())
}

/// Parses help text for the command at `command_path` (e.g. ["tool",
/// "deploy"]). The returned node is marked `Described` only when something
/// usable was found.
pub fn parse_help(text: &str, command_path: &[&str]) -> CommandNode {
    let lines = preprocess(text);
    let mut node = CommandNode {
        name: command_path.last().copied().unwrap_or_default().to_string(),
        status: NodeStatus::Unavailable,
        ..Default::default()
    };
    let mut section = Section::None;
    let mut usage = UsageInfo::default();
    let mut pending_flag: Option<(FlagLine, usize, Vec<Choice>, bool)> = None;
    let mut sub_indent: Option<usize> = None;
    let mut last_sub: Option<usize> = None;
    // A command row without a description must be followed by an indented
    // description (man `.TP` items); otherwise it was a prose fragment.
    let mut bare_sub = false;
    let mut argparse_choices: Option<(Vec<String>, usize)> = None;

    let flush = |pending: &mut Option<(FlagLine, usize, Vec<Choice>, bool)>, node: &mut CommandNode| {
        if let Some((fl, _, choices, _)) = pending.take() {
            let flag = build_flag(fl, choices);
            if !flag.names.iter().any(|n| node.flag(n).is_some()) {
                node.flags.push(flag);
            }
        }
    };

    for line in &lines {
        let indent = indentation(line);
        let trimmed = line.trim();

        // Usage lines can appear anywhere ("Usage: x" or "usage: x").
        let lower = trimmed.to_ascii_lowercase();
        if indent <= 2 && (lower.starts_with("usage:") || lower.starts_with("usage of ")) {
            flush(&mut pending_flag, &mut node);
            let rest = trimmed[6..].trim();
            if lower.starts_with("usage:") && !rest.is_empty() {
                usage.forms.push(rest.to_string());
            }
            section = Section::Usage;
            continue;
        }

        let continues_command =
            section == Section::Commands && !trimmed.is_empty() && sub_indent.is_some_and(|e| indent > e);
        if std::mem::take(&mut bare_sub) && !continues_command {
            node.subcommands.pop();
            last_sub = None;
        }

        if trimmed.is_empty() {
            // A blank line ends a flag's continuation unless the flag still
            // has no description (Go-style help puts it on the next line).
            if pending_flag.as_ref().is_some_and(|p| !p.0.description.is_empty()) {
                flush(&mut pending_flag, &mut node);
            }
            last_sub = None;
            continue;
        }

        if let Some(title) = header_title(line) {
            flush(&mut pending_flag, &mut node);
            section = classify_header(title);
            sub_indent = None;
            last_sub = None;
            argparse_choices = None;
            continue;
        }

        match section {
            Section::Usage => {
                // Additional usage forms start with the program name; any
                // other line ends the usage block.
                let first = trimmed.split_whitespace().next().unwrap_or("");
                let program = command_path.first().copied().unwrap_or("");
                if indent > 0 && !program.is_empty() && (first == program || first.ends_with(&format!("/{program}"))) {
                    usage.forms.push(trimmed.to_string());
                    continue;
                }
                // A wrapped usage form continues on indented bracket lines.
                if indent > 0
                    && trimmed.starts_with(['[', '<', '{', '('])
                    && let Some(last) = usage.forms.last_mut()
                {
                    last.push(' ');
                    last.push_str(trimmed);
                    continue;
                }
                section = Section::Other;
            }
            Section::Examples => continue,
            _ => {}
        }

        // Continuation of the previous flag's description.
        if let Some((fl, flag_indent, choices, in_possible)) = pending_flag.as_mut()
            && indent > *flag_indent
            && !trimmed.starts_with('-')
        {
            if trimmed.to_ascii_lowercase().starts_with("possible values") {
                *in_possible = true;
            } else if *in_possible {
                if let Some(item) = trimmed.strip_prefix("- ") {
                    let (v, d) = match item.split_once(':') {
                        Some((v, d)) => (v.trim(), Some(display_text(d, MAX_DESCRIPTION))),
                        None => (item.trim(), None),
                    };
                    if is_valid_word(v) {
                        choices.push(Choice {
                            value: v.to_string(),
                            description: d,
                        });
                    }
                }
            } else if fl.description.len() < 600 {
                if !fl.description.is_empty() {
                    fl.description.push(' ');
                }
                fl.description.push_str(trimmed);
            }
            continue;
        }

        if trimmed.starts_with('-')
            && section != Section::Commands
            && indent <= 24
            && let Some(fl) = parse_flag_line(trimmed)
        {
            flush(&mut pending_flag, &mut node);
            pending_flag = Some((fl, indent, Vec::new(), false));
            continue;
        }
        flush(&mut pending_flag, &mut node);

        // argparse lists subcommands as a {a,b,c} positional.
        if matches!(section, Section::Arguments | Section::Commands)
            && trimmed.starts_with('{')
            && trimmed.ends_with('}')
            && let Some(list) = parse_choice_list(&trimmed[1..trimmed.len() - 1])
        {
            for n in &list {
                if node.subcommand(n).is_none() {
                    node.subcommands.push(CommandNode {
                        name: n.clone(),
                        ..Default::default()
                    });
                }
            }
            node.expects_subcommand = true;
            argparse_choices = Some((list, indent));
            continue;
        }
        if let Some((list, base)) = &argparse_choices
            && indent > *base
        {
            if let Some(sl) = parse_subcommand_line(trimmed)
                && let Some(sub) = node
                    .subcommands
                    .iter_mut()
                    .find(|s| list.contains(&s.name) && s.name == sl.names[0])
                && !sl.description.is_empty()
            {
                sub.description = Some(display_text(&sl.description, MAX_DESCRIPTION));
            }
            continue;
        }

        if section == Section::Commands && indent >= 1 {
            let entry_indent = *sub_indent.get_or_insert(indent);
            if indent > entry_indent {
                // continuation of the previous subcommand description
                if let Some(i) = last_sub {
                    let sub = &mut node.subcommands[i];
                    if sub.description.is_none() {
                        sub.description = Some(display_text(trimmed, MAX_DESCRIPTION));
                    }
                }
                continue;
            }
            if indent == entry_indent {
                // yargs/commander list commands as "prog sub [args]  desc".
                let mut row = trimmed;
                for word in command_path {
                    match row.strip_prefix(word).and_then(|r| r.strip_prefix(' ')) {
                        Some(r) if !r.starts_with(' ') => row = r,
                        _ => break,
                    }
                }
                if let Some(sl) = parse_subcommand_line(row) {
                    let name = sl.names[0].clone();
                    if node.subcommand(&name).is_none() {
                        bare_sub = sl.description.is_empty();
                        node.subcommands.push(CommandNode {
                            name,
                            aliases: sl.names[1..].to_vec(),
                            description: (!sl.description.is_empty())
                                .then(|| display_text(&sl.description, MAX_DESCRIPTION)),
                            ..Default::default()
                        });
                        last_sub = Some(node.subcommands.len() - 1);
                    }
                    continue;
                }
            }
            last_sub = None;
        }
    }
    flush(&mut pending_flag, &mut node);
    if bare_sub {
        node.subcommands.pop();
    }

    // Options that appear only in usage lines complement the option table.
    for flag in usage_flags(&usage) {
        if !flag.names.iter().any(|n| node.flag(n).is_some()) {
            node.flags.push(flag);
        }
    }
    let path_words: Vec<&str> = command_path.to_vec();
    let (mut expects_sub, mut positionals) = parse_usage_forms(&usage, &path_words);
    if node.subcommands.is_empty() {
        let literals = usage_literal_subcommands(&usage, &path_words);
        if !literals.is_empty() {
            for name in literals {
                node.subcommands.push(CommandNode {
                    name,
                    ..Default::default()
                });
            }
            expects_sub = true;
            positionals.clear();
        }
    }
    node.expects_subcommand |= expects_sub || !node.subcommands.is_empty() && expects_sub;
    if !expects_sub || node.subcommands.is_empty() {
        node.positionals = positionals;
    }
    if node.has_content() {
        node.status = NodeStatus::Described;
    }
    node
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::ValueKind;

    fn fixture(name: &str) -> String {
        let path = format!("{}/tests/fixtures/help/dev/{name}", env!("CARGO_MANIFEST_DIR"));
        String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned()
    }

    fn flag<'a>(n: &'a CommandNode, name: &str) -> &'a Flag {
        n.flag(name)
            .unwrap_or_else(|| panic!("missing flag {name}; have {:?}", all_flag_names(n)))
    }

    fn all_flag_names(n: &CommandNode) -> Vec<String> {
        n.flags.iter().flat_map(|f| f.names.clone()).collect()
    }

    fn sub_names(n: &CommandNode) -> Vec<&str> {
        n.subcommands.iter().map(|s| s.name.as_str()).collect()
    }

    fn choices(f: &Flag) -> Vec<&str> {
        f.value
            .as_ref()
            .unwrap()
            .choices
            .iter()
            .map(|c| c.value.as_str())
            .collect()
    }

    #[test]
    fn clap_root_and_subcommand() {
        let n = parse_help(&fixture("clap_root.txt"), &["deployer"]);
        assert_eq!(n.status, NodeStatus::Described);
        assert_eq!(sub_names(&n), ["deploy", "rollback", "status", "help"]);
        assert!(n.expects_subcommand);
        assert_eq!(
            all_flag_names(&n),
            [
                "-c",
                "--config",
                "-v",
                "--verbose",
                "--color",
                "-h",
                "--help",
                "-V",
                "--version"
            ]
        );
        let config = flag(&n, "--config");
        assert_eq!(config.value.as_ref().unwrap().kind, ValueKind::Path);
        assert!(flag(&n, "-v").repeatable);
        assert!(flag(&n, "-v").value.is_none());
        assert_eq!(choices(flag(&n, "--color")), ["auto", "always", "never"]);
        assert_eq!(
            n.subcommand("rollback").unwrap().description.as_deref(),
            Some("Roll back to a previous release")
        );

        let d = parse_help(&fixture("clap_deploy.txt"), &["deployer", "deploy"]);
        assert_eq!(choices(flag(&d, "-e")), ["dev", "staging", "prod"]);
        assert!(flag(&d, "--dry-run").value.is_none());
        assert_eq!(
            flag(&d, "--tag").value.as_ref().unwrap().placeholder.as_deref(),
            Some("<TAG>")
        );
        assert_eq!(flag(&d, "--jobs").value.as_ref().unwrap().kind, ValueKind::Number);
        assert!(d.subcommands.is_empty());
        assert_eq!(d.positionals.len(), 1);
        assert_eq!(d.positionals[0].name, "TARGET");
        assert!(d.positionals[0].variadic && d.positionals[0].optional);
    }

    #[test]
    fn argparse_subparsers_and_choices() {
        let n = parse_help(&fixture("argparse.txt"), &["pyt"]);
        assert_eq!(sub_names(&n), ["init", "build", "serve"]);
        assert_eq!(
            n.subcommand("build").unwrap().description.as_deref(),
            Some("Build the project")
        );
        assert_eq!(choices(flag(&n, "--format")), ["json", "yaml", "text"]);
        assert_eq!(flag(&n, "--format").description.as_deref(), Some("Output format"));
        let out = flag(&n, "-o");
        assert_eq!(out.names, ["-o", "--output"]);
        assert_eq!(out.value.as_ref().unwrap().placeholder.as_deref(), Some("OUTPUT"));
        assert!(flag(&n, "-q").value.is_none());
        assert!(n.expects_subcommand);
    }

    #[test]
    fn click_style() {
        let n = parse_help(&fixture("click.txt"), &["clk"]);
        assert_eq!(sub_names(&n), ["publish", "sync"]);
        assert_eq!(choices(flag(&n, "--level")), ["debug", "info", "warning"]);
        assert_eq!(flag(&n, "--dry-run").names, ["--dry-run", "--no-dry-run"]);
        assert!(flag(&n, "--dry-run").value.is_none());
        assert_eq!(
            flag(&n, "-n").value.as_ref().unwrap().placeholder.as_deref(),
            Some("TEXT")
        );
        assert!(n.expects_subcommand);
    }

    #[test]
    fn cobra_style() {
        let n = parse_help(&fixture("cobra.txt"), &["kc"]);
        assert_eq!(sub_names(&n), ["apply", "completion", "get", "help"]);
        assert_eq!(choices(flag(&n, "-o")), ["json", "yaml", "wide"]);
        assert_eq!(flag(&n, "--timeout").value.as_ref().unwrap().kind, ValueKind::Number);
        assert_eq!(flag(&n, "--context").value.as_ref().unwrap().kind, ValueKind::Any);
        assert!(n.expects_subcommand);
    }

    #[test]
    fn gnu_style_and_prose_is_not_flags() {
        let n = parse_help(&fixture("gnu.txt"), &["gls"]);
        assert_eq!(
            all_flag_names(&n),
            [
                "-a",
                "--all",
                "-B",
                "--ignore-backups",
                "--block-size",
                "--color",
                "-I",
                "--ignore",
                "--sort",
                "--help"
            ]
        );
        let color = flag(&n, "--color");
        assert!(color.value_optional);
        assert!(!color.takes_separate_value());
        assert!(flag(&n, "-I").takes_separate_value());
        assert_eq!(
            flag(&n, "--block-size").description.as_deref(),
            Some("with -l, scale sizes by SIZE when printing them; e.g., '--block-size=M'; see SIZE format below")
        );
        // Prose mentions (--force-all, -x, -1) must not become flags.
        assert!(n.flag("--force-all").is_none());
        assert!(n.flag("-x").is_none());
        assert!(n.flag("-1").is_none());
        assert!(n.subcommands.is_empty());
        assert_eq!(n.positionals.len(), 1);
        assert_eq!(n.positionals[0].value.kind, ValueKind::Path);
    }

    #[test]
    fn go_flag_style() {
        let n = parse_help(&fixture("goflag.txt"), &["gotool"]);
        assert_eq!(all_flag_names(&n), ["-addr", "-debug", "-mode", "-n"]);
        assert_eq!(
            flag(&n, "-addr").description.as_deref(),
            Some("listen address (default \":8080\")")
        );
        assert!(flag(&n, "-debug").value.is_none());
        assert_eq!(choices(flag(&n, "-mode")), ["fast", "safe"]);
        assert_eq!(flag(&n, "-n").value.as_ref().unwrap().kind, ValueKind::Number);
    }

    #[test]
    fn commander_style() {
        let n = parse_help(&fixture("commander.txt"), &["nodetool"]);
        assert_eq!(sub_names(&n), ["init", "run", "help"]);
        assert_eq!(n.subcommand("r").unwrap().name, "run");
        assert_eq!(choices(flag(&n, "--env")), ["dev", "prod"]);
    }

    #[test]
    fn hostile_help_is_sanitized_and_not_trusted() {
        let n = parse_help(&fixture("hostile.txt"), &["evil"]);
        let names = all_flag_names(&n);
        assert!(names.contains(&"--good-one".to_string()));
        assert!(names.contains(&"--xflag".to_string()));
        for bad in ["--$(id)", "--`whoami`", "-;rm", "--$", "--"] {
            assert!(!names.contains(&bad.to_string()), "{bad} accepted: {names:?}");
        }
        for f in &n.flags {
            for name in &f.names {
                assert!(
                    name.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_.:+@?#".contains(c)),
                    "{name}"
                );
            }
            if let Some(d) = &f.description {
                assert!(!d.chars().any(crate::sanitize::is_forbidden_char), "{d:?}");
            }
        }
        assert_eq!(sub_names(&n), ["ok-cmd"]);
    }

    #[test]
    fn garbage_and_empty_input() {
        assert_eq!(parse_help("", &["x"]).status, NodeStatus::Unavailable);
        let garbage: String = (0..5000u32).filter_map(char::from_u32).collect();
        let n = parse_help(&garbage, &["x"]);
        assert!(n.flags.len() < 5);
        let n = parse_help("just some prose\nwithout any table at all\n", &["x"]);
        assert_eq!(n.status, NodeStatus::Unavailable);
    }

    #[test]
    fn grouped_command_lists_with_long_header() {
        let n = parse_help(&fixture("grouped_commands.txt"), &["vcsx"]);
        assert_eq!(sub_names(&n), ["clone", "init", "add", "rm"]);
        assert!(n.expects_subcommand);
        assert_eq!(
            n.subcommand("rm").unwrap().description.as_deref(),
            Some("Remove files from the working tree")
        );
    }

    #[test]
    fn options_from_usage_lines() {
        let n = parse_help(&fixture("grouped_commands.txt"), &["vcsx"]);
        assert!(flag(&n, "-C").takes_separate_value());
        assert_eq!(flag(&n, "-v").names, ["-v", "--version"]);
        assert!(flag(&n, "--help").value.is_none());
        let text =
            "usage: sshy [-46Aq] [-B bind_interface] [-p port] [--exec-path[=<path>]] [-x | --long=<v>] destination\n";
        let n = parse_help(text, &["sshy"]);
        for c in ["-4", "-6", "-A", "-q"] {
            assert!(flag(&n, c).value.is_none(), "{c}");
        }
        assert!(n.flag("-46Aq").is_none());
        assert!(flag(&n, "-p").takes_separate_value());
        assert!(flag(&n, "--exec-path").value_optional);
        assert_eq!(flag(&n, "-x").names, ["-x", "--long"]);
        assert!(flag(&n, "-B").takes_separate_value());
        // the documented table wins over usage-line duplicates
        let n = parse_help(&fixture("clap_root.txt"), &["deployer"]);
        assert_eq!(
            n.flags
                .iter()
                .filter(|f| f.names.contains(&"--config".to_string()))
                .count(),
            1
        );
    }

    #[test]
    fn prose_in_command_sections_is_not_a_command() {
        let text = "COMMANDS\n       Set\n       HEAD to the given value. See the\n       output.\n       Interrogators:\n\n       real-cmd\n              Does real things\n";
        let n = parse_help(text, &["x"]);
        assert_eq!(sub_names(&n), ["real-cmd"]);
    }

    #[test]
    fn unicode_subcommands_and_choices() {
        let text = "Usage: uni <COMMAND>\n\nCommands:\n  prüfen   Prüft alles\n  検査     Check\n\nOptions:\n  --modus <MODUS>  Modus [possible values: schnell, gründlich]\n";
        let n = parse_help(text, &["uni"]);
        assert_eq!(sub_names(&n), ["prüfen", "検査"]);
        assert_eq!(choices(flag(&n, "--modus")), ["schnell", "gründlich"]);
        assert!(!is_valid_word("a\u{202e}b"));
        assert!(!is_valid_word("a b"));
        assert!(!is_valid_word("$x"));
    }

    #[test]
    fn ordering_is_deterministic() {
        let a = parse_help(&fixture("cobra.txt"), &["kc"]);
        let b = parse_help(&fixture("cobra.txt"), &["kc"]);
        assert_eq!(a, b);
    }
}
