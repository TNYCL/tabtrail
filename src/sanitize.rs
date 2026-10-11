//! Handling of untrusted text: help/man output, executable names, provider
//! output and candidate strings are all treated as hostile data.

/// Removes ANSI/VT escape sequences and backspace overstrike sequences.
pub fn strip_escapes(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                Some('[') => {
                    // CSI: parameters/intermediates then a final byte @..~
                    for n in chars.by_ref() {
                        if ('@'..='~').contains(&n) {
                            break;
                        }
                    }
                }
                Some(']') | Some('P') | Some('X') | Some('^') | Some('_') => {
                    // OSC/DCS/SOS/PM/APC: terminated by BEL or ST (ESC \)
                    while let Some(n) = chars.next() {
                        if n == '\x07' {
                            break;
                        }
                        if n == '\x1b' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                Some('(') | Some(')') | Some('*') | Some('+') => {
                    chars.next();
                }
                _ => {}
            },
            '\u{9b}' => {
                // 8-bit CSI
                for n in chars.by_ref() {
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            }
            '\x08' => {
                // overstrike: "X\bX" (bold) or "_\bX" (underline) -> X
                out.pop();
            }
            _ => out.push(c),
        }
    }
    out
}

/// Characters that must never reach a terminal inside candidates or display
/// text: C0/C1 controls, DEL, and bidirectional formatting overrides.
pub fn is_forbidden_char(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
}

/// Makes untrusted text safe and compact for display: escapes are removed,
/// every whitespace run becomes one space, forbidden characters are dropped,
/// and the result is truncated to `max_chars`.
pub fn display_text(input: &str, max_chars: usize) -> String {
    let stripped = strip_escapes(input);
    let mut out = String::with_capacity(stripped.len().min(max_chars * 4));
    let mut count = 0;
    let mut pending_space = false;
    for c in stripped.chars() {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if is_forbidden_char(c) {
            continue;
        }
        if pending_space {
            if count + 1 >= max_chars {
                break;
            }
            out.push(' ');
            count += 1;
            pending_space = false;
        }
        if count >= max_chars {
            break;
        }
        out.push(c);
        count += 1;
    }
    if count >= max_chars && max_chars > 1 {
        out.pop();
        out.push('…');
    }
    out
}

/// Escapes control and bidi characters (`\u{..}`) so diagnostic output of
/// untrusted values (paths, names, help-derived text) cannot drive the
/// terminal. Ordinary text, including non-ASCII letters, is kept.
pub fn escape_display(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        if is_forbidden_char(c) {
            out.push_str(&format!("\\u{{{:x}}}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

/// A candidate's insertion text must not contain control or bidi characters
/// (they cannot be typed back safely and may manipulate the terminal).
pub fn is_safe_candidate(s: &str) -> bool {
    !s.is_empty() && s.len() <= 1024 && !s.chars().any(is_forbidden_char)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_csi_osc_and_overstrike() {
        assert_eq!(strip_escapes("\x1b[1;31mred\x1b[0m"), "red");
        assert_eq!(strip_escapes("a\x1b]0;title\x07b"), "ab");
        assert_eq!(strip_escapes("a\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\b"), "alinkb");
        assert_eq!(strip_escapes("B\x08Bo\x08ol\x08ld"), "Bold");
        assert_eq!(strip_escapes("_\x08u_\x08n"), "un");
        assert_eq!(strip_escapes("x\u{9b}2Jy"), "xy");
    }

    #[test]
    fn display_text_removes_controls_and_bidi_and_truncates() {
        assert_eq!(display_text("  multi\n line\ttext  ", 100), "multi line text");
        assert_eq!(display_text("evil\u{202e}txt.exe\x07\x1b[2J", 100), "eviltxt.exe");
        let long = "a".repeat(50);
        let t = display_text(&long, 10);
        assert_eq!(t.chars().count(), 10);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn escape_display_neutralises_terminal_controls() {
        assert_eq!(escape_display("/a\nb\x1b]0;x\x07ü"), "/a\\u{a}b\\u{1b}]0;x\\u{7}ü");
        assert_eq!(escape_display("plain/path"), "plain/path");
    }

    #[test]
    fn candidate_safety() {
        assert!(is_safe_candidate("--flag"));
        assert!(is_safe_candidate("ünïcode-ok"));
        assert!(!is_safe_candidate(""));
        assert!(!is_safe_candidate("a\nb"));
        assert!(!is_safe_candidate("a\tb"));
        assert!(!is_safe_candidate("\x1b[31mx"));
        assert!(!is_safe_candidate("x\u{202e}y"));
    }
}
