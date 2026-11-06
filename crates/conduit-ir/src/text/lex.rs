//! Splits text into tokens. Line comments start with `//`.

use super::Error;

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Tok {
    /// A keyword, a name like `add` or `sum#3`, an ID like `fn1` or
    /// `data10`, or a mutability parameter like `'m1`.
    Ident(String),
    /// What follows the `%` of a variable: `3`, `acc` or `acc#3`.
    Var(String),
    /// A number, with its suffix if it has one: `5`, `-3i64`, `1.5e-3f32`.
    Num(String),
    Str(String),
    /// A byte string, `b"hi\n"`, as its bytes.
    Bytes(Vec<u8>),
    /// One of `( ) { } [ ] , : = . @`, or `=>` or `->`.
    Punct(&'static str),
    Eof,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Pos {
    pub line: u32,
    pub col: u32,
}

pub(super) fn lex(src: &str) -> Result<Vec<(Tok, Pos)>, Error> {
    let chars: Vec<char> = src.chars().collect();
    let mut toks: Vec<(Tok, Pos)> = Vec::new();
    let (mut i, mut line, mut col) = (0, 1, 1);
    let err = |pos: Pos, message: String| Error {
        line: pos.line,
        col: pos.col,
        message,
    };

    while i < chars.len() {
        let c = chars[i];
        let pos = Pos { line, col };
        let start = i;
        if c == '\n' {
            i += 1;
            line += 1;
            col = 1;
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            col += 1;
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        let tok = if c == 'b' && chars.get(i + 1) == Some(&'"') {
            i += 2;
            Tok::Bytes(byte_string(&chars, &mut i, pos)?)
        } else if c.is_ascii_alphabetic()
            || c == '_'
            || (c == '\'' && chars.get(i + 1).is_some_and(|c| c.is_ascii_alphabetic()))
        {
            // A mutability parameter like `'m1` is a name too, quote and all.
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            i = number_suffix(&chars, i);
            Tok::Ident(chars[start..i].iter().collect())
        } else if c == '%' {
            i += 1;
            let name = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            if i == name {
                return Err(err(
                    pos,
                    "expected a variable after `%`, like `%3` or `%acc`".into(),
                ));
            }
            i = number_suffix(&chars, i);
            Tok::Var(chars[name..i].iter().collect())
        } else if c.is_ascii_digit()
            || (c == '-' && chars.get(i + 1).is_some_and(char::is_ascii_digit))
        {
            // After a `.`, as in `data10.0.1`, a number is a plain index:
            // `0.1` there isn't a float.
            let after_dot = matches!(toks.last(), Some((Tok::Punct("."), _)));
            i += 1;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            if !after_dot {
                if chars.get(i) == Some(&'.') && chars.get(i + 1).is_some_and(char::is_ascii_digit)
                {
                    i += 1;
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                if matches!(chars.get(i), Some('e' | 'E')) {
                    let sign = usize::from(matches!(chars.get(i + 1), Some('+' | '-')));
                    if chars.get(i + 1 + sign).is_some_and(char::is_ascii_digit) {
                        i += 1 + sign;
                        while i < chars.len() && chars[i].is_ascii_digit() {
                            i += 1;
                        }
                    }
                }
                let rest: String = chars[i..chars.len().min(i + 3)].iter().collect();
                if matches!(rest.as_str(), "i16" | "i32" | "i64" | "f32" | "f64") {
                    i += 3;
                } else if rest.starts_with("i8") {
                    i += 2;
                }
            }
            Tok::Num(chars[start..i].iter().collect())
        } else if c == '"' {
            i += 1;
            let mut s = String::new();
            loop {
                match chars.get(i) {
                    None | Some('\n') => return Err(err(pos, "unterminated string".into())),
                    Some('"') => break,
                    Some('\\') => {
                        let escaped = match chars.get(i + 1) {
                            Some('"') => '"',
                            Some('\\') => '\\',
                            Some('n') => '\n',
                            Some('t') => '\t',
                            None | Some('\n') => {
                                return Err(err(pos, "unterminated string".into()));
                            }
                            _ => return Err(err(pos, "unknown escape in a string".into())),
                        };
                        s.push(escaped);
                        i += 2;
                    }
                    Some(&c) => {
                        s.push(c);
                        i += 1;
                    }
                }
            }
            i += 1;
            Tok::Str(s)
        } else {
            let two: String = chars[i..chars.len().min(i + 2)].iter().collect();
            let punct = match two.as_str() {
                "=>" => "=>",
                "->" => "->",
                _ => match c {
                    '(' => "(",
                    ')' => ")",
                    '{' => "{",
                    '}' => "}",
                    '[' => "[",
                    ']' => "]",
                    ',' => ",",
                    ';' => ";",
                    ':' => ":",
                    '=' => "=",
                    '.' => ".",
                    '@' => "@",
                    '+' => "+",
                    _ => return Err(err(pos, format!("unexpected character `{c}`"))),
                },
            };
            i += punct.len();
            Tok::Punct(punct)
        };
        col += (i - start) as u32;
        toks.push((tok, pos));
    }
    toks.push((Tok::Eof, Pos { line, col }));
    Ok(toks)
}

/// After a name, the end of `#3` if there is one, or `i`.
fn number_suffix(chars: &[char], i: usize) -> usize {
    if chars.get(i) == Some(&'#') && chars.get(i + 1).is_some_and(char::is_ascii_digit) {
        let mut j = i + 1;
        while j < chars.len() && chars[j].is_ascii_digit() {
            j += 1;
        }
        j
    } else {
        i
    }
}

/// The bytes of a byte string, from just after its `b"` to its closing
/// `"`, which `i` is left after. Characters other than escapes are their
/// UTF-8 bytes.
fn byte_string(chars: &[char], i: &mut usize, pos: Pos) -> Result<Vec<u8>, Error> {
    let err = |message: &str| Error {
        line: pos.line,
        col: pos.col,
        message: message.into(),
    };
    let mut bytes = Vec::new();
    loop {
        match chars.get(*i) {
            None | Some('\n') => return Err(err("unterminated byte string")),
            Some('"') => break,
            Some('\\') => {
                let escaped = match chars.get(*i + 1) {
                    Some('"') => b'"',
                    Some('\\') => b'\\',
                    Some('n') => b'\n',
                    Some('t') => b'\t',
                    Some('r') => b'\r',
                    Some('0') => 0,
                    Some('x') => {
                        let digits = chars.get(*i + 2..*i + 4).unwrap_or_default();
                        if digits.len() != 2 || !digits.iter().all(char::is_ascii_hexdigit) {
                            return Err(err("`\\x` needs two hex digits, like `\\x7f`"));
                        }
                        let hex: String = digits.iter().collect();
                        *i += 2;
                        u8::from_str_radix(&hex, 16).unwrap()
                    }
                    None | Some('\n') => return Err(err("unterminated byte string")),
                    _ => return Err(err("unknown escape in a byte string")),
                };
                bytes.push(escaped);
                *i += 2;
            }
            Some(&c) => {
                let mut buf = [0; 4];
                bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                *i += 1;
            }
        }
    }
    *i += 1;
    Ok(bytes)
}
