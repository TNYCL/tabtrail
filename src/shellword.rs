//! Shell-aware tokenizer for the incomplete text before the cursor.
//!
//! The tokenizer never evaluates anything. It dequotes words, records which
//! raw byte each logical character came from, and marks words that contain
//! expansions (variables, globs, command substitutions, ...) so later stages
//! can treat them as opaque. Syntax it cannot interpret safely (cursor inside a
//! command substitution, heredoc bodies, comments) yields `Unsupported`, which
//! callers turn into "no Tabtrail candidates" so the shell's own behaviour wins.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Bash,
    Zsh,
    Fish,
}

impl Dialect {
    fn posix(self) -> bool {
        !matches!(self, Dialect::Fish)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quote {
    Single,
    Double,
    /// Bash/Zsh `$'...'`.
    AnsiC,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// `|`, `|&`
    Pipe,
    /// `&&`, `||`, `;`, `&`, newline, `;;` and friends.
    Separator,
    OpenParen,
    CloseParen,
    /// Any redirection operator; the following word is its target.
    Redirect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word {
    /// Raw byte span in the input.
    pub start: usize,
    pub end: usize,
    /// Dequoted value. Expansions are kept literally.
    pub value: String,
    /// Raw byte offset of every logical character of `value`.
    pub offsets: Vec<usize>,
    /// Contains an expansion whose result is unknown without evaluation.
    pub expansion: bool,
    /// Contains a command or process substitution (a subset of `expansion`).
    /// Some shells' own file completion evaluates these, so callers must not
    /// delegate to it for such words.
    pub subst: bool,
    /// Any quoting (quotes or backslashes) was used.
    pub quoted: bool,
    /// Unterminated quote at the end of input: kind and raw byte offset of
    /// the opening quote character.
    pub open_quote: Option<(Quote, usize)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    Word(Word),
    Op { op: Op, start: usize, end: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tokens {
    pub tokens: Vec<Token>,
    /// The input ends inside a word (no trailing unquoted separator), i.e.
    /// the last word is the one being completed.
    pub ends_in_word: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unsupported(pub &'static str);

struct Lexer<'a> {
    chars: Vec<(usize, char)>,
    pos: usize,
    input: &'a str,
    dialect: Dialect,
}

impl Lexer<'_> {
    fn peek(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.pos + ahead).map(|&(_, c)| c)
    }
    fn offset(&self) -> usize {
        self.chars.get(self.pos).map(|&(o, _)| o).unwrap_or(self.input.len())
    }

    /// Skips a balanced construct starting at the current position (which
    /// must be just after the opening delimiter). Returns Err when the input
    /// ends before the construct is closed.
    fn skip_balanced(&mut self, open: char, close: char) -> Result<(), Unsupported> {
        let mut depth = 1usize;
        while let Some(c) = self.peek(0) {
            self.pos += 1;
            match c {
                '\\' => {
                    self.pos += 1;
                }
                '\'' => self.skip_until('\'')?,
                '"' => self.skip_dquote()?,
                '`' if self.dialect.posix() => self.skip_until('`')?,
                c if c == open => depth += 1,
                c if c == close => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                _ => {}
            }
        }
        Err(Unsupported("cursor inside an unterminated substitution or group"))
    }

    fn skip_until(&mut self, end: char) -> Result<(), Unsupported> {
        while let Some(c) = self.peek(0) {
            self.pos += 1;
            if c == '\\' && end != '\'' {
                self.pos += 1;
            } else if c == end {
                return Ok(());
            }
        }
        Err(Unsupported("cursor inside an unterminated quoted substitution"))
    }

    fn skip_dquote(&mut self) -> Result<(), Unsupported> {
        while let Some(c) = self.peek(0) {
            self.pos += 1;
            match c {
                '\\' => self.pos += 1,
                '"' => return Ok(()),
                '$' if self.peek(0) == Some('(') => {
                    self.pos += 1;
                    self.skip_balanced('(', ')')?;
                }
                '`' if self.dialect.posix() => self.skip_until('`')?,
                _ => {}
            }
        }
        Err(Unsupported("cursor inside an unterminated quoted substitution"))
    }
}

fn is_blank(c: char) -> bool {
    c == ' ' || c == '\t'
}

fn is_word_end(c: char, dialect: Dialect) -> bool {
    match c {
        ' ' | '\t' | '\n' | ';' | '|' | '&' | '<' | '>' => true,
        ')' => true,
        '(' => dialect.posix(),
        _ => false,
    }
}

/// Tokenizes `input` (normally: the command line up to the cursor).
pub fn tokenize(input: &str, dialect: Dialect) -> Result<Tokens, Unsupported> {
    let mut lx = Lexer {
        chars: input.char_indices().collect(),
        pos: 0,
        input,
        dialect,
    };
    let mut tokens = Vec::new();
    let mut ends_in_word = false;

    while let Some(c) = lx.peek(0) {
        ends_in_word = false;
        let start = lx.offset();
        // Line continuation.
        if c == '\\' && lx.peek(1) == Some('\n') {
            lx.pos += 2;
            continue;
        }
        if is_blank(c) {
            lx.pos += 1;
            continue;
        }
        if c == '#' {
            // A comment only starts at the beginning of a word.
            while let Some(c) = lx.peek(0) {
                if c == '\n' {
                    break;
                }
                lx.pos += 1;
            }
            if lx.peek(0).is_none() {
                return Err(Unsupported("cursor inside a comment"));
            }
            continue;
        }
        if let Some((op, len)) = operator_at(&lx, dialect) {
            if op == Op::Redirect && lx.chars[lx.pos..].iter().take(len).any(|&(_, c)| c == '<') {
                let text: String = lx.chars[lx.pos..lx.pos + len].iter().map(|&(_, c)| c).collect();
                if text.ends_with("<<") || text.ends_with("<<-") {
                    // A heredoc: everything after the next newline is its body.
                    let rest: String = lx.chars[lx.pos + len..].iter().map(|&(_, c)| c).collect();
                    if rest.contains('\n') {
                        return Err(Unsupported("heredoc body"));
                    }
                }
            }
            lx.pos += len;
            tokens.push(Token::Op {
                op,
                start,
                end: lx.offset(),
            });
            continue;
        }
        let before = lx.pos;
        let word = lex_word(&mut lx)?;
        if lx.pos == before {
            // Defensive: never loop without progress on unexpected input.
            lx.pos += 1;
            continue;
        }
        ends_in_word = lx.peek(0).is_none();
        tokens.push(Token::Word(word));
    }
    Ok(Tokens { tokens, ends_in_word })
}

/// Recognizes an operator at the current position, including redirections
/// with a leading file-descriptor number such as `2>` or `2>&1`'s `2>&`.
fn operator_at(lx: &Lexer<'_>, dialect: Dialect) -> Option<(Op, usize)> {
    let at = |i: usize| lx.peek(i);
    // Leading digits immediately followed by < or > form a redirection.
    let mut digits = 0;
    while at(digits).is_some_and(|c| c.is_ascii_digit()) {
        digits += 1;
    }
    let c0 = at(digits)?;
    if digits > 0 && c0 != '<' && c0 != '>' {
        return None;
    }
    if digits > 0 {
        return redirect_len(lx, digits).map(|l| (Op::Redirect, l));
    }
    match c0 {
        '\n' | ';' => {
            let len = if at(1) == Some(';') && dialect.posix() { 2 } else { 1 };
            Some((Op::Separator, len))
        }
        '|' => match at(1) {
            Some('|') => Some((Op::Separator, 2)),
            Some('&') if dialect.posix() => Some((Op::Pipe, 2)),
            _ => Some((Op::Pipe, 1)),
        },
        '&' => match at(1) {
            Some('&') => Some((Op::Separator, 2)),
            Some('>') => redirect_len(lx, 0).map(|l| (Op::Redirect, l)),
            Some('|') if dialect == Dialect::Fish => Some((Op::Pipe, 2)),
            _ => Some((Op::Separator, 1)),
        },
        '<' | '>' => {
            if dialect.posix() && at(1) == Some('(') {
                return None; // process substitution: part of a word
            }
            redirect_len(lx, 0).map(|l| (Op::Redirect, l))
        }
        '(' if dialect.posix() => Some((Op::OpenParen, 1)),
        ')' => Some((Op::CloseParen, 1)),
        _ => None,
    }
}

fn redirect_len(lx: &Lexer<'_>, skip: usize) -> Option<usize> {
    let at = |i: usize| lx.peek(skip + i);
    let mut len = 0;
    if at(0) == Some('&') {
        len += 1; // &> or &>>
    }
    match at(len)? {
        '<' => {
            len += 1;
            while at(len) == Some('<') && len < 3 {
                len += 1;
            }
            if at(len) == Some('>') || at(len) == Some('&') || at(len) == Some('-') {
                len += 1;
            }
        }
        '>' => {
            len += 1;
            if matches!(at(len), Some('>') | Some('|') | Some('&') | Some('?')) {
                len += 1;
            }
            if at(len) == Some('&') {
                len += 1;
            }
        }
        _ => return None,
    }
    Some(skip + len)
}

fn push(word: &mut Word, c: char, raw: usize) {
    word.value.push(c);
    word.offsets.push(raw);
}

fn lex_word(lx: &mut Lexer<'_>) -> Result<Word, Unsupported> {
    let dialect = lx.dialect;
    let start = lx.offset();
    let mut w = Word {
        start,
        end: start,
        value: String::new(),
        offsets: Vec::new(),
        expansion: false,
        subst: false,
        quoted: false,
        open_quote: None,
    };
    let at_word_start = |w: &Word| w.value.is_empty() && !w.quoted;

    while let Some(c) = lx.peek(0) {
        let raw = lx.offset();
        let proc_subst = dialect.posix() && (c == '<' || c == '>') && lx.peek(1) == Some('(');
        if is_word_end(c, dialect) && !proc_subst {
            // `(` inside a word (zsh glob qualifiers, bash extglob) is part
            // of a glob pattern rather than a subshell.
            if c == '(' && !w.value.is_empty() {
                lx.pos += 1;
                lx.skip_balanced('(', ')')?;
                w.expansion = true;
                continue;
            }
            break;
        }
        match c {
            '\\' => {
                w.quoted = true;
                match lx.peek(1) {
                    None => {
                        // Trailing backslash: an escape still being typed.
                        lx.pos += 1;
                    }
                    Some('\n') => lx.pos += 2,
                    Some(n) => {
                        lx.pos += 2;
                        if dialect == Dialect::Fish {
                            match fish_escape(n) {
                                Some(ch) => push(&mut w, ch, raw),
                                None => {
                                    // \x, \u, \c ... sequences: value unknown here.
                                    w.expansion = true;
                                    push(&mut w, n, raw);
                                }
                            }
                        } else {
                            push(&mut w, n, raw);
                        }
                    }
                }
            }
            '\'' => {
                w.quoted = true;
                lx.pos += 1;
                lex_single(lx, &mut w, raw)?;
            }
            '"' => {
                w.quoted = true;
                lx.pos += 1;
                lex_double(lx, &mut w, raw)?;
            }
            '$' if dialect.posix() && lx.peek(1) == Some('\'') => {
                w.quoted = true;
                lx.pos += 2;
                lex_ansi_c(lx, &mut w, raw)?;
            }
            '$' if dialect.posix() && lx.peek(1) == Some('"') => {
                w.quoted = true;
                lx.pos += 2;
                lex_double(lx, &mut w, raw)?;
            }
            '$' => {
                w.expansion = true;
                lx.pos += 1;
                lex_dollar(lx, &mut w, raw)?;
            }
            '`' if dialect.posix() => {
                w.expansion = true;
                w.subst = true;
                lx.pos += 1;
                lx.skip_until('`')?;
            }
            '(' => {
                // Fish command substitution.
                w.expansion = true;
                w.subst = true;
                lx.pos += 1;
                lx.skip_balanced('(', ')')?;
            }
            '<' | '>' => {
                // Bash/Zsh process substitution <(...) / >(...).
                w.expansion = true;
                w.subst = true;
                lx.pos += 2;
                lx.skip_balanced('(', ')')?;
            }
            '*' | '?' | '[' => {
                w.expansion = true;
                push(&mut w, c, raw);
                lx.pos += 1;
            }
            '{' => {
                // A lone `{` word is a reserved word, otherwise brace expansion.
                let lone = at_word_start(&w) && lx.peek(1).is_none_or(|n| is_word_end(n, dialect) || n == '\n');
                if !lone {
                    w.expansion = true;
                }
                push(&mut w, c, raw);
                lx.pos += 1;
            }
            '~' if at_word_start(&w) => {
                w.expansion = true;
                push(&mut w, c, raw);
                lx.pos += 1;
            }
            '=' if at_word_start(&w) && dialect == Dialect::Zsh => {
                // zsh EQUALS expansion (=cmd)
                w.expansion = true;
                push(&mut w, c, raw);
                lx.pos += 1;
            }
            '!' if dialect.posix() && !w.value.is_empty() => {
                // history expansion such as foo!! or !$ inside a word
                w.expansion = true;
                push(&mut w, c, raw);
                lx.pos += 1;
            }
            _ => {
                push(&mut w, c, raw);
                lx.pos += 1;
            }
        }
    }
    w.end = lx.offset();
    Ok(w)
}

fn fish_escape(n: char) -> Option<char> {
    match n {
        'n' => Some('\n'),
        't' => Some('\t'),
        'r' => Some('\r'),
        'e' => Some('\x1b'),
        'a' => Some('\x07'),
        'b' => Some('\x08'),
        'f' => Some('\x0c'),
        'v' => Some('\x0b'),
        'x' | 'X' | 'u' | 'U' | 'c' | '0'..='7' => None,
        other => Some(other),
    }
}

fn lex_single(lx: &mut Lexer<'_>, w: &mut Word, open_at: usize) -> Result<(), Unsupported> {
    while let Some(c) = lx.peek(0) {
        let raw = lx.offset();
        if c == '\'' {
            lx.pos += 1;
            return Ok(());
        }
        if lx.dialect == Dialect::Fish
            && c == '\\'
            && let Some(n @ ('\'' | '\\')) = lx.peek(1)
        {
            lx.pos += 2;
            push(w, n, raw);
            continue;
        }
        push(w, c, raw);
        lx.pos += 1;
    }
    w.open_quote = Some((Quote::Single, open_at));
    Ok(())
}

fn lex_double(lx: &mut Lexer<'_>, w: &mut Word, open_at: usize) -> Result<(), Unsupported> {
    while let Some(c) = lx.peek(0) {
        let raw = lx.offset();
        match c {
            '"' => {
                lx.pos += 1;
                return Ok(());
            }
            '\\' => match lx.peek(1) {
                Some(n @ ('"' | '\\' | '$' | '`')) => {
                    lx.pos += 2;
                    push(w, n, raw);
                }
                Some('\n') => lx.pos += 2,
                Some(_) => {
                    push(w, '\\', raw);
                    lx.pos += 1;
                }
                None => lx.pos += 1,
            },
            '$' => {
                w.expansion = true;
                lx.pos += 1;
                lex_dollar(lx, w, raw)?;
            }
            '`' if lx.dialect.posix() => {
                w.expansion = true;
                w.subst = true;
                lx.pos += 1;
                lx.skip_until('`')?;
            }
            '!' if lx.dialect.posix() => {
                w.expansion = true;
                push(w, c, raw);
                lx.pos += 1;
            }
            _ => {
                push(w, c, raw);
                lx.pos += 1;
            }
        }
    }
    w.open_quote = Some((Quote::Double, open_at));
    Ok(())
}

fn lex_ansi_c(lx: &mut Lexer<'_>, w: &mut Word, open_at: usize) -> Result<(), Unsupported> {
    while let Some(c) = lx.peek(0) {
        let raw = lx.offset();
        match c {
            '\'' => {
                lx.pos += 1;
                return Ok(());
            }
            '\\' => {
                let Some(n) = lx.peek(1) else {
                    lx.pos += 1;
                    continue;
                };
                lx.pos += 2;
                let decoded = match n {
                    'n' => Some('\n'),
                    't' => Some('\t'),
                    'r' => Some('\r'),
                    'e' | 'E' => Some('\x1b'),
                    'a' => Some('\x07'),
                    'b' => Some('\x08'),
                    'f' => Some('\x0c'),
                    'v' => Some('\x0b'),
                    '\\' | '\'' | '"' | '?' => Some(n),
                    _ => None,
                };
                match decoded {
                    Some(ch) => push(w, ch, raw),
                    None => {
                        // Numeric escapes: value not decoded here.
                        w.expansion = true;
                        push(w, n, raw);
                    }
                }
            }
            _ => {
                push(w, c, raw);
                lx.pos += 1;
            }
        }
    }
    w.open_quote = Some((Quote::AnsiC, open_at));
    Ok(())
}

/// Handles what follows a `$` (already consumed): `$(...)`, `$((...))`,
/// `${...}`, or a plain name. Expansion text is kept literally in the value.
fn lex_dollar(lx: &mut Lexer<'_>, w: &mut Word, raw: usize) -> Result<(), Unsupported> {
    push(w, '$', raw);
    match lx.peek(0) {
        Some('(') => {
            w.subst = true;
            lx.pos += 1;
            lx.skip_balanced('(', ')')
        }
        Some('{') if lx.dialect.posix() => {
            lx.pos += 1;
            lx.skip_balanced('{', '}')
        }
        _ => {
            while let Some(c) = lx.peek(0) {
                if c.is_alphanumeric() || c == '_' {
                    push(w, c, lx.offset());
                    lx.pos += 1;
                } else {
                    break;
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(input: &str, d: Dialect) -> Vec<String> {
        tokenize(input, d)
            .unwrap()
            .tokens
            .into_iter()
            .map(|t| match t {
                Token::Word(w) => w.value,
                Token::Op { start, end, .. } => format!("<op:{}>", &input[start..end]),
            })
            .collect()
    }

    fn last_word(input: &str, d: Dialect) -> Word {
        match tokenize(input, d).unwrap().tokens.pop().unwrap() {
            Token::Word(w) => w,
            other => panic!("expected word, got {other:?}"),
        }
    }

    #[test]
    fn splits_plain_words_and_operators() {
        assert_eq!(
            words("a b|c && d; e 2>/dev/null >>log", Dialect::Bash),
            [
                "a",
                "b",
                "<op:|>",
                "c",
                "<op:&&>",
                "d",
                "<op:;>",
                "e",
                "<op:2>>",
                "/dev/null",
                "<op:>>>",
                "log"
            ]
        );
    }

    #[test]
    fn dequotes_single_double_and_backslash() {
        assert_eq!(
            words(r#"cmd 'a b' "c \"d\" \$e" f\ g"#, Dialect::Bash),
            ["cmd", "a b", r#"c "d" $e"#, "f g"]
        );
    }

    #[test]
    fn ansi_c_quotes_in_bash() {
        let w = last_word(r"cmd $'a\tb\'c'", Dialect::Bash);
        assert_eq!(w.value, "a\tb'c");
        assert!(!w.expansion);
    }

    #[test]
    fn records_raw_offsets_for_each_logical_char() {
        let input = r#"cmd --env="pr o""#;
        let w = last_word(input, Dialect::Bash);
        assert_eq!(w.value, "--env=pr o");
        let eq = w.value.find('=').unwrap();
        assert_eq!(&input[w.offsets[eq]..w.offsets[eq] + 1], "=");
        // 'p' sits after the opening quote
        assert_eq!(&input[w.offsets[eq + 1]..w.offsets[eq + 1] + 1], "p");
    }

    #[test]
    fn unicode_offsets_are_byte_offsets() {
        let input = "cmd héllo wörld";
        let w = last_word(input, Dialect::Bash);
        assert_eq!(w.value, "wörld");
        assert_eq!(w.start, "cmd héllo ".len());
        assert_eq!(w.offsets[2], "cmd héllo wö".len());
    }

    #[test]
    fn open_quote_is_reported() {
        let w = last_word("cmd --env='pr", Dialect::Bash);
        assert_eq!(w.value, "--env=pr");
        assert_eq!(w.open_quote, Some((Quote::Single, "cmd --env=".len())));
        let w = last_word("cmd \"my f", Dialect::Zsh);
        assert_eq!(w.open_quote, Some((Quote::Double, 4)));
        assert_eq!(w.value, "my f");
    }

    #[test]
    fn ends_in_word_tracks_trailing_space() {
        assert!(tokenize("cmd ar", Dialect::Bash).unwrap().ends_in_word);
        assert!(!tokenize("cmd ar ", Dialect::Bash).unwrap().ends_in_word);
        assert!(tokenize("cmd ar\\ ", Dialect::Bash).unwrap().ends_in_word);
        assert!(tokenize("cmd 'a ", Dialect::Bash).unwrap().ends_in_word);
    }

    #[test]
    fn expansions_are_marked_not_evaluated() {
        for (input, d) in [
            ("cmd $HOME", Dialect::Bash),
            ("cmd \"$(rm -rf /)\"", Dialect::Bash),
            ("cmd `id`", Dialect::Bash),
            ("cmd *.txt", Dialect::Bash),
            ("cmd ~/x", Dialect::Bash),
            ("cmd {a,b}", Dialect::Bash),
            ("cmd <(ls)", Dialect::Bash),
            ("cmd (id)", Dialect::Fish),
            ("cmd $(id)", Dialect::Fish),
            ("cmd =ls", Dialect::Zsh),
            ("cmd a!!", Dialect::Bash),
            ("cmd ${x}", Dialect::Zsh),
        ] {
            let w = last_word(input, d);
            assert!(w.expansion, "{input:?} should be marked as expansion");
        }
        assert!(!last_word("cmd plain-word", Dialect::Bash).expansion);
    }

    #[test]
    fn substitutions_are_flagged_separately_from_other_expansions() {
        for (input, d) in [
            ("cmd --a=$(id)", Dialect::Bash),
            ("cmd \"x`id`\"", Dialect::Zsh),
            ("cmd <(ls)", Dialect::Bash),
            ("cmd --a=(id)", Dialect::Fish),
            ("cmd $(id)", Dialect::Fish),
        ] {
            let w = last_word(input, d);
            assert!(w.expansion && w.subst, "{input}");
        }
        for input in ["cmd $HOME", "cmd *.txt", "cmd ~/x"] {
            let w = last_word(input, Dialect::Bash);
            assert!(w.expansion && !w.subst, "{input}");
        }
    }

    #[test]
    fn closed_substitution_is_one_opaque_word() {
        assert_eq!(words("cmd $(echo a; echo b) next", Dialect::Bash), ["cmd", "$", "next"]);
    }

    #[test]
    fn unterminated_substitution_is_unsupported() {
        for (input, d) in [
            ("cmd $(git sw", Dialect::Bash),
            ("cmd `git sw", Dialect::Bash),
            ("cmd \"$(git sw", Dialect::Zsh),
            ("cmd (git sw", Dialect::Fish),
            ("cmd ${HO", Dialect::Bash),
        ] {
            assert!(tokenize(input, d).is_err(), "{input:?}");
        }
    }

    #[test]
    fn comments_and_heredocs() {
        assert!(tokenize("cmd # comment", Dialect::Bash).is_err());
        assert_eq!(words("cmd # c\nnext a", Dialect::Bash), ["cmd", "<op:\n>", "next", "a"]);
        assert!(tokenize("cat <<EOF\nbody", Dialect::Bash).is_err());
        assert!(tokenize("cat <<<word", Dialect::Bash).is_ok());
        // '#' inside a word is not a comment
        assert_eq!(words("cmd a#b", Dialect::Bash), ["cmd", "a#b"]);
    }

    #[test]
    fn fish_quoting_rules() {
        assert_eq!(words(r"cmd 'it\'s' a\ b", Dialect::Fish), ["cmd", "it's", "a b"]);
        // fish has no $'...' quoting; `$` starts a variable
        assert!(last_word("cmd $'x'", Dialect::Fish).expansion);
    }

    #[test]
    fn redirection_variants() {
        assert_eq!(
            words("cmd &>out 2>&1 <in", Dialect::Bash),
            ["cmd", "<op:&>>", "out", "<op:2>&>", "1", "<op:<>", "in"]
        );
        // a digit-only word not followed by a redirect stays a word
        assert_eq!(words("cmd 42 x", Dialect::Bash), ["cmd", "42", "x"]);
    }

    #[test]
    fn line_continuation_is_skipped() {
        assert_eq!(words("cmd a \\\n b", Dialect::Bash), ["cmd", "a", "b"]);
    }

    #[test]
    fn hostile_input_does_not_panic() {
        for input in [
            "\\",
            "'",
            "\"",
            "$",
            "$(",
            "${",
            "`",
            "<(",
            "2>",
            "&",
            "\u{0}",
            "\u{1b}[31m",
            "a\\\u{e9}",
            "$'\\",
            "((((",
            "))))",
            "{{{",
            "}",
            "\"\\",
            "'\\",
        ] {
            for d in [Dialect::Bash, Dialect::Zsh, Dialect::Fish] {
                let _ = tokenize(input, d);
            }
        }
    }
}
