//! Splits text into tokens. Line comments start with `//`.

use super::Error;

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Tok {
    /// A keyword, a name like `add`, or an ID like `fn1` or `data10`.
    Ident(String),
    /// `%3`
    Var(u32),
    /// A number, with its suffix if it has one: `5`, `-3i64`, `1.5e-3f32`.
    Num(String),
    Str(String),
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

        let tok = if c.is_ascii_alphabetic() || c == '_' {
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            Tok::Ident(chars[start..i].iter().collect())
        } else if c == '%' {
            i += 1;
            let digits = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            let text: String = chars[digits..i].iter().collect();
            let n = text
                .parse()
                .map_err(|_| err(pos, "expected a variable number after `%`".into()))?;
            Tok::Var(n)
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
                if matches!(rest.as_str(), "i32" | "i64" | "f32" | "f64") {
                    i += 3;
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
                    ':' => ":",
                    '=' => "=",
                    '.' => ".",
                    '@' => "@",
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
