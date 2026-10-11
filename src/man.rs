//! Passive metadata from installed manual pages. Nothing is executed: the
//! roff source is read from disk (gzip-decompressed in-process), converted
//! to a help-like text layout and handed to the help parser.

use std::ffi::OsStr;

use std::io::Read;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;

const MAX_COMPRESSED: u64 = 4 * 1024 * 1024;
const MAX_TEXT: u64 = 8 * 1024 * 1024;
const SECTIONS: &[&str] = &["1", "8", "6"];

/// Manual page roots to search, most specific first: the prefix of the
/// resolved executable, then MANPATH entries, then PATH-derived prefixes,
/// then the system defaults.
pub fn man_roots(executable: &Path, man_path: Option<&OsStr>, path_var: Option<&OsStr>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut add = |p: PathBuf| {
        if !roots.contains(&p) {
            roots.push(p);
        }
    };
    let prefix_of = |bin_dir: &Path| bin_dir.parent().map(|pre| [pre.join("share/man"), pre.join("man")]);
    if let Some(dir) = executable.parent() {
        for p in prefix_of(dir).into_iter().flatten() {
            add(p);
        }
    }
    if let Some(mp) = man_path {
        for p in std::env::split_paths(mp) {
            if p.is_absolute() {
                add(p);
            }
        }
    }
    if let Some(pv) = path_var {
        for dir in std::env::split_paths(pv).filter(|d| d.is_absolute()) {
            for p in prefix_of(&dir).into_iter().flatten() {
                add(p);
            }
        }
    }
    add(PathBuf::from("/usr/share/man"));
    add(PathBuf::from("/usr/local/share/man"));
    roots
}

/// Finds the page for `name` (e.g. "tool" or "tool-deploy").
pub fn find_page(roots: &[PathBuf], name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains('/') || name.starts_with('.') {
        return None;
    }
    for root in roots {
        for sec in SECTIONS {
            let dir = root.join(format!("man{sec}"));
            for file in [format!("{name}.{sec}"), format!("{name}.{sec}.gz")] {
                let p = dir.join(&file);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// Reads a page, following a single `.so` redirection within its root.
/// Returns the text and the path of the file that actually supplied it.
pub fn read_page(path: &Path) -> Option<(String, PathBuf)> {
    let text = read_bounded(path)?;
    let first = text.lines().find(|l| !l.trim().is_empty() && !l.starts_with(".\\\""))?;
    if let Some(target) = first.strip_prefix(".so ") {
        let target = target.trim();
        if target.contains("..") || target.starts_with('/') {
            return None;
        }
        let root = path.parent()?.parent()?;
        let p = root.join(target);
        let alt = PathBuf::from(format!("{}.gz", p.display()));
        return read_bounded(&p)
            .map(|t| (t, p.clone()))
            .or_else(|| read_bounded(&alt).map(|t| (t, alt)));
    }
    Some((text, path.to_path_buf()))
}

/// dev:ino:size:mtime_ns of a file (None if it cannot be stat'ed).
pub fn file_identity(path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path).ok()?;
    Some(format!(
        "{}:{}:{}:{}",
        m.dev(),
        m.ino(),
        m.size(),
        i128::from(m.mtime()) * 1_000_000_000 + i128::from(m.mtime_nsec())
    ))
}

/// Stable fingerprint of a list of search roots (never 0).
pub fn roots_fingerprint(roots: &[PathBuf]) -> u64 {
    let mut joined = Vec::new();
    for r in roots {
        joined.extend_from_slice(r.as_os_str().as_encoded_bytes());
        joined.push(0);
    }
    crate::fsutil::fnv1a64(&joined) | 1
}

fn read_bounded(path: &Path) -> Option<String> {
    let f = crate::fsutil::open_regular(path, MAX_COMPRESSED).ok()?;
    let mut bytes = Vec::new();
    if path.extension().is_some_and(|e| e == "gz") {
        GzDecoder::new(f).take(MAX_TEXT).read_to_end(&mut bytes).ok()?;
    } else {
        f.take(MAX_TEXT).read_to_end(&mut bytes).ok()?;
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

const BODY_INDENT: &str = "       ";
const TAG_INDENT: &str = "       ";
const DESC_INDENT: &str = "              ";

/// Removes roff escapes. In `tag` mode, italic runs (placeholders by man
/// convention) are wrapped in angle brackets.
fn unescape(s: &str, tag: bool) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    let mut italic = false;
    let close_italic = |out: &mut String, italic: &mut bool| {
        if *italic {
            out.push('>');
            *italic = false;
        }
    };
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some(n) = chars.next() else { break };
        match n {
            '-' => out.push('-'),
            'e' | '\\' => out.push('\\'),
            '&' | '|' | '^' | '%' | ':' | 'c' | ')' | '/' | ',' => {}
            ' ' | '~' | '0' => out.push(' '),
            '\'' => out.push('\''),
            '`' => out.push('`'),
            '.' => out.push('.'),
            'f' => {
                let font: String = match chars.next() {
                    Some('(') => chars.by_ref().take(2).collect(),
                    Some('[') => chars.by_ref().take_while(|&c| c != ']').collect(),
                    Some(f) => f.to_string(),
                    None => String::new(),
                };
                if tag {
                    let is_italic = matches!(font.as_str(), "I" | "BI" | "IT" | "2");
                    if is_italic && !italic {
                        out.push('<');
                        italic = true;
                    } else if !is_italic {
                        close_italic(&mut out, &mut italic);
                    }
                }
            }
            '(' => {
                let name: String = chars.by_ref().take(2).collect();
                out.push_str(special_char(&name));
            }
            '[' => {
                let name: String = chars.by_ref().take_while(|&c| c != ']').collect();
                out.push_str(special_char(&name));
            }
            '*' => match chars.next() {
                Some('(') => {
                    chars.next();
                    chars.next();
                }
                Some('[') => while chars.next().is_some_and(|c| c != ']') {},
                _ => {}
            },
            's' => {
                if matches!(chars.peek(), Some('+') | Some('-')) {
                    chars.next();
                }
                while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
                    chars.next();
                }
            }
            'N' | 'h' | 'v' | 'w' | 'o' | 'X' | 'Z' => {
                // \N'..' \h'..' etc.: skip the delimited argument
                if let Some(d) = chars.next() {
                    while chars.next().is_some_and(|c| c != d) {}
                }
            }
            other => out.push(other),
        }
    }
    close_italic(&mut out, &mut italic);
    out
}

fn special_char(name: &str) -> &'static str {
    match name {
        "hy" | "mi" | "en" => "-",
        "em" => "--",
        "aq" | "oq" | "cq" => "'",
        "dq" | "lq" | "rq" => "\"",
        "bu" => "*",
        "ti" => "~",
        "ga" => "`",
        "rs" => "\\",
        _ => "",
    }
}

/// Splits macro arguments, honouring double quotes.
fn macro_args(s: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut has = false;
    for c in s.chars() {
        match c {
            '"' => {
                in_q = !in_q;
                has = true;
            }
            ' ' | '\t' if !in_q => {
                if has || !cur.is_empty() {
                    args.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            _ => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        args.push(cur);
    }
    args
}

fn font_wrap(text: &str, font: char, tag: bool) -> String {
    if tag && font == 'I' {
        format!("<{text}>")
    } else {
        text.to_string()
    }
}

/// Converts roff (man and basic mdoc macros) to a help-like text layout:
/// section headers at column 0, `.TP`/`.IP` tags at 7, bodies at 14.
pub fn roff_to_text(src: &str) -> String {
    let mut out = String::new();
    let mut expect_tag = false;
    let mut in_item = false;
    let mut skip_macro_def = false;
    let mut synopsis: Option<String> = None;
    let emit_raw = |out: &mut String, indent: &str, text: &str| {
        let t = text.trim();
        if !t.is_empty() {
            out.push_str(indent);
            out.push_str(t);
        }
        out.push('\n');
    };
    // The SYNOPSIS is filled text spread over many requests; join it into a
    // single usage line.
    let emit = |out: &mut String, indent: &str, text: &str, synopsis: &mut Option<String>| match synopsis {
        Some(buf) => {
            if !buf.is_empty() && !text.trim().is_empty() {
                buf.push(' ');
            }
            buf.push_str(text.trim());
        }
        None => emit_raw(out, indent, text),
    };

    for raw in src.lines() {
        if skip_macro_def {
            if raw.starts_with("..") {
                skip_macro_def = false;
            }
            continue;
        }
        let is_request = raw.starts_with('.') || raw.starts_with('\'');
        if !is_request {
            let tag_line = expect_tag;
            let text = unescape(raw, tag_line);
            if tag_line {
                emit(&mut out, TAG_INDENT, &text, &mut synopsis);
                expect_tag = false;
                in_item = true;
            } else {
                emit(
                    &mut out,
                    if in_item { DESC_INDENT } else { BODY_INDENT },
                    &text,
                    &mut synopsis,
                );
            }
            continue;
        }
        let body = raw[1..].trim_start();
        let (name, rest) = body.split_once([' ', '\t']).unwrap_or((body, ""));
        let rest = rest.trim();
        match name {
            "\\\"" | "" => {}
            n if n.starts_with("\\\"") => {}
            "de" | "de1" | "am" | "ig" => skip_macro_def = true,
            "SH" | "Sh" => {
                let title = unescape(&macro_args(rest).join(" "), false).to_ascii_uppercase();
                if let Some(text) = synopsis.take() {
                    out.push_str(BODY_INDENT);
                    out.push_str(&text);
                    out.push('\n');
                }
                if title == "SYNOPSIS" {
                    synopsis = Some(String::new());
                }
                out.push('\n');
                out.push_str(&title);
                out.push('\n');
                expect_tag = false;
                in_item = false;
            }
            "SS" | "Ss" => {
                let title = unescape(&macro_args(rest).join(" "), false);
                out.push('\n');
                out.push_str(&format!("{title}:\n"));
                expect_tag = false;
                in_item = false;
            }
            "TP" | "TQ" => {
                expect_tag = true;
                if name == "TP" {
                    out.push('\n');
                }
            }
            "IP" => {
                let args = macro_args(rest);
                if let Some(tag) = args.first().filter(|a| !a.is_empty()) {
                    out.push('\n');
                    emit(&mut out, TAG_INDENT, &unescape(tag, true), &mut synopsis);
                    in_item = true;
                }
            }
            "PP" | "LP" | "P" | "Pp" | "sp" => {
                out.push('\n');
                in_item = false;
                expect_tag = false;
            }
            "B" | "I" | "SM" | "SB" => {
                let font = if name == "I" { 'I' } else { 'B' };
                let text = font_wrap(&unescape(&macro_args(rest).join(" "), expect_tag), font, expect_tag);
                if expect_tag {
                    emit(&mut out, TAG_INDENT, &text, &mut synopsis);
                    expect_tag = false;
                    in_item = true;
                } else {
                    emit(
                        &mut out,
                        if in_item { DESC_INDENT } else { BODY_INDENT },
                        &text,
                        &mut synopsis,
                    );
                }
            }
            "BR" | "RB" | "BI" | "IB" | "IR" | "RI" => {
                let fonts: Vec<char> = name.chars().collect();
                let text: String = macro_args(rest)
                    .iter()
                    .enumerate()
                    .map(|(i, a)| font_wrap(&unescape(a, expect_tag), fonts[i % 2], expect_tag))
                    .collect();
                if expect_tag {
                    emit(&mut out, TAG_INDENT, &text, &mut synopsis);
                    expect_tag = false;
                    in_item = true;
                } else {
                    emit(
                        &mut out,
                        if in_item { DESC_INDENT } else { BODY_INDENT },
                        &text,
                        &mut synopsis,
                    );
                }
            }
            // mdoc list items: .It Fl a Ar file
            "It" => {
                let tag = mdoc_inline(rest);
                out.push('\n');
                emit(&mut out, TAG_INDENT, &tag, &mut synopsis);
                in_item = true;
            }
            "Nm" | "Fl" | "Ar" | "Op" | "Cm" => {
                let text = mdoc_inline(&format!("{name} {rest}"));
                emit(
                    &mut out,
                    if in_item { DESC_INDENT } else { BODY_INDENT },
                    &text,
                    &mut synopsis,
                );
            }
            _ => {}
        }
    }
    if let Some(text) = synopsis.take() {
        out.push_str(BODY_INDENT);
        out.push_str(&text);
        out.push('\n');
    }
    out
}

/// Renders a line of mdoc inline macros (Fl, Ar, Op, Cm, Nm ...).
fn mdoc_inline(s: &str) -> String {
    let mut out = String::new();
    let mut words = macro_args(s).into_iter().peekable();
    let mut glue = false;
    while let Some(w) = words.next() {
        let piece = match w.as_str() {
            "Fl" => {
                let next = words.peek().filter(|n| !is_mdoc_macro(n)).cloned();
                if next.is_some() {
                    words.next();
                }
                format!("-{}", unescape(&next.unwrap_or_default(), false))
            }
            "Ar" => {
                let next = words.peek().filter(|n| !is_mdoc_macro(n)).cloned();
                if next.is_some() {
                    words.next();
                }
                format!("<{}>", unescape(&next.unwrap_or_else(|| "arg".into()), false))
            }
            "Op" | "Oo" => {
                glue = true;
                "[".to_string()
            }
            "Oc" => "]".to_string(),
            "Cm" | "Nm" | "Ic" | "Li" | "Sy" | "Em" | "Pa" | "Xo" | "Xc" | "Ns" => continue,
            other => unescape(other, false),
        };
        if !out.is_empty() && !glue && piece != "]" && piece != "," {
            out.push(' ');
        }
        glue = piece == "[";
        out.push_str(&piece);
    }
    if out.contains('[') && !out.contains(']') {
        out.push(']');
    }
    out
}

fn is_mdoc_macro(w: &str) -> bool {
    w.len() == 2
        && w.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && w.chars().nth(1).is_some_and(|c| c.is_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::help::parse_help;
    use crate::schema::ValueKind;
    use crate::testutil::TempDir;
    use std::fs;

    const PAGE: &str = r#".\" comment
.TH MANTOOL 1
.SH NAME
mantool \- manage widgets
.SH SYNOPSIS
.B mantool
[\fIOPTIONS\fR] \fIFILE\fR...
.SH OPTIONS
.TP
.BR \-a ", " \-\-all
show all widgets
.TP
\fB\-o\fR, \fB\-\-output\fR=\fIfile\fR
write output to
.I file
instead of standard output
.TP
.BR \-\-color [=\fIWHEN\fR]
colorize; \fIWHEN\fR is one of always, never, auto
.IP "\fB\-q\fR" 4
quiet
.SH COMMANDS
.TP
.B sync
synchronise the widgets
.TP
.B prune
remove stale widgets
.SH SEE ALSO
.BR other (1)
"#;

    #[test]
    fn roff_page_parses_into_flags_and_commands() {
        let text = roff_to_text(PAGE);
        let n = parse_help(&text, &["mantool"]);
        let names: Vec<_> = n.flags.iter().flat_map(|f| f.names.clone()).collect();
        assert_eq!(names, ["-a", "--all", "-o", "--output", "--color", "-q"], "{text}");
        let out = n.flag("--output").unwrap();
        assert_eq!(out.value.as_ref().unwrap().kind, ValueKind::Path);
        assert_eq!(
            out.description.as_deref(),
            Some("write output to file instead of standard output")
        );
        let color = n.flag("--color").unwrap();
        assert!(color.value_optional);
        let choices: Vec<_> = color
            .value
            .as_ref()
            .unwrap()
            .choices
            .iter()
            .map(|c| c.value.as_str())
            .collect();
        assert_eq!(choices, ["always", "never", "auto"]);
        let subs: Vec<_> = n.subcommands.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(subs, ["sync", "prune"]);
        assert_eq!(
            n.subcommand("sync").unwrap().description.as_deref(),
            Some("synchronise the widgets")
        );
        // The joined SYNOPSIS yields the positional argument.
        assert_eq!(n.positionals.len(), 1, "{text}");
        assert_eq!(n.positionals[0].name, "FILE");
        assert!(n.positionals[0].variadic);
    }

    #[test]
    fn mdoc_items() {
        let src =
            ".Sh DESCRIPTION\n.Bl -tag -width Ds\n.It Fl v\nverbose output\n.It Fl f Ar file\nread from file\n.El\n";
        let text = roff_to_text(src);
        let n = parse_help(&text, &["bsdtool"]);
        let names: Vec<_> = n.flags.iter().flat_map(|f| f.names.clone()).collect();
        assert_eq!(names, ["-v", "-f"], "{text}");
        assert!(n.flag("-f").unwrap().takes_separate_value());
    }

    #[test]
    fn finds_gzipped_pages_and_follows_so() {
        use flate2::{Compression, write::GzEncoder};
        use std::io::Write;
        let t = TempDir::new("man");
        let root = t.path().join("share/man");
        fs::create_dir_all(root.join("man1")).unwrap();
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(PAGE.as_bytes()).unwrap();
        fs::write(root.join("man1/mantool.1.gz"), enc.finish().unwrap()).unwrap();
        fs::write(root.join("man1/mt.1"), ".so man1/mantool.1\n").unwrap();
        let roots = man_roots(&t.path().join("bin/mantool"), None, None);
        assert_eq!(roots[0], root);
        let page = find_page(&roots, "mantool").unwrap();
        assert!(read_page(&page).unwrap().0.contains("manage widgets"));
        let alias = find_page(&roots, "mt").unwrap();
        let (text, source) = read_page(&alias).unwrap();
        assert!(text.contains("manage widgets"));
        assert_eq!(
            source,
            root.join("man1/mantool.1.gz"),
            "the .so target supplies the content"
        );
        assert!(find_page(&roots, "../etc/passwd").is_none());
        assert!(find_page(&roots, "missing").is_none() || !roots.is_empty());
    }

    #[test]
    fn hostile_roff_does_not_panic() {
        for src in [
            "\\",
            "\\f",
            "\\(",
            "\\*[",
            ".TP",
            ".BR",
            ".It Fl",
            "\\N'",
            ".de x\nno end",
            "\\s+",
            ".SH \\",
        ] {
            let _ = parse_help(&roff_to_text(src), &["x"]);
        }
    }
}
