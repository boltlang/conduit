//! The checks in a test program: comment lines starting with `//#`.

use conduit_ir::Lit;

/// A check, and the line it's on.
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    /// 1-based.
    pub line: u32,
    pub kind: CheckKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CheckKind {
    /// `//# run fn1(5i32) == 120i32`, or `traps` instead of `== ..`. The
    /// function is as the text writes it: `fn1`, `sum` or `sum#1`.
    Run {
        func: String,
        args: Vec<Lit>,
        expect: Expect,
    },
    /// `//# verify-error: text`: the verifier rejects the program with a
    /// message containing `text`.
    VerifyError(String),
    /// `//# wasm-error: text`: the Wasm backend can't lower the program,
    /// with a message containing `text`.
    WasmError(String),
    /// `//# wasm-skip: reason`: the Wasm backend doesn't run the program.
    WasmSkip(String),
    /// `//# wat`: the Wasm text of the program matches `<name>.wat`.
    Wat,
    /// `//# opt`: the program after optimization matches
    /// `<name>.opt.conduit`. `//# opt: dce, copies` runs only the passes
    /// with those names, in that order, instead of all of them.
    Opt(Option<Vec<String>>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expect {
    Values(Vec<Lit>),
    Trap,
}

/// The checks in a program's text, or the line of the first one that
/// doesn't parse and why.
pub fn parse(text: &str) -> Result<Vec<Check>, (u32, String)> {
    let mut checks = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line_no = i as u32 + 1;
        let Some(rest) = line.trim_start().strip_prefix("//#") else {
            continue;
        };
        let kind = check(rest.trim()).map_err(|e| (line_no, e))?;
        checks.push(Check {
            line: line_no,
            kind,
        });
    }
    Ok(checks)
}

fn check(s: &str) -> Result<CheckKind, String> {
    let (word, rest) = s.split_once([' ', ':']).unwrap_or((s, ""));
    let message = |rest: &str| -> Result<String, String> {
        match rest.trim() {
            "" => Err(format!("`{word}` needs a message after `:`")),
            m => Ok(m.to_string()),
        }
    };
    let none = |kind: CheckKind| {
        if rest.trim().is_empty() {
            Ok(kind)
        } else {
            Err(format!("`{word}` takes nothing after it"))
        }
    };
    match word {
        "run" => run(rest),
        "verify-error" => Ok(CheckKind::VerifyError(message(rest)?)),
        "wasm-error" => Ok(CheckKind::WasmError(message(rest)?)),
        "wasm-skip" => Ok(CheckKind::WasmSkip(message(rest)?)),
        "wat" => none(CheckKind::Wat),
        "opt" => opt(rest),
        _ => Err(format!(
            "unknown check `{word}`: expected `run`, `verify-error`, `wasm-error`, \
             `wasm-skip`, `wat` or `opt`"
        )),
    }
}

/// Nothing, or `: name, ..`.
fn opt(s: &str) -> Result<CheckKind, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(CheckKind::Opt(None));
    }
    let names: Vec<String> = s.split(',').map(|n| n.trim().to_string()).collect();
    if names.iter().any(|n| n.is_empty()) {
        return Err("expected pass names separated by `,`, like `opt: dce, copies`".into());
    }
    Ok(CheckKind::Opt(Some(names)))
}

/// `fn1(5i32, 1i64) == 120i32`, `== (1i32, 2i32)`, `== ()` or `traps`.
fn run(s: &str) -> Result<CheckKind, String> {
    let mut c = Cursor { s: s.trim() };
    let func = c.word().to_string();
    if func.is_empty() {
        return Err("expected a function like `fn1` or `sum`".into());
    }
    let args = c.lits()?;
    let expect = if c.eat("==") {
        if c.peek_is('(') {
            Expect::Values(c.lits()?)
        } else {
            Expect::Values(vec![c.lit()?])
        }
    } else if c.word() == "traps" {
        Expect::Trap
    } else {
        return Err("expected `== results` or `traps` after the call".into());
    };
    if !c.s.trim().is_empty() {
        return Err(format!("unexpected `{}` at the end", c.s.trim()));
    }
    Ok(CheckKind::Run { func, args, expect })
}

struct Cursor<'a> {
    s: &'a str,
}

impl Cursor<'_> {
    fn skip_space(&mut self) {
        self.s = self.s.trim_start();
    }

    fn peek_is(&mut self, c: char) -> bool {
        self.skip_space();
        self.s.starts_with(c)
    }

    fn eat(&mut self, p: &str) -> bool {
        self.skip_space();
        match self.s.strip_prefix(p) {
            Some(rest) => {
                self.s = rest;
                true
            }
            None => false,
        }
    }

    /// Letters, digits, `_`, `.`, `+` and `-`: a name or a literal.
    fn word(&mut self) -> &str {
        self.skip_space();
        let end = self
            .s
            .find(|c: char| !(c.is_alphanumeric() || "_.+-".contains(c)))
            .unwrap_or(self.s.len());
        let (word, rest) = self.s.split_at(end);
        self.s = rest;
        word
    }

    /// `(lit, ..)`
    fn lits(&mut self) -> Result<Vec<Lit>, String> {
        if !self.eat("(") {
            return Err("expected `(`".into());
        }
        let mut out = Vec::new();
        if self.eat(")") {
            return Ok(out);
        }
        loop {
            out.push(self.lit()?);
            if self.eat(")") {
                return Ok(out);
            }
            if !self.eat(",") {
                return Err("expected `,` or `)`".into());
            }
        }
    }

    /// A literal as the text format writes it: `5i8`, `5i32`, `-1i64`, `1.5f32`,
    /// `1e300f64`, `f32_bits(2143289344)`.
    fn lit(&mut self) -> Result<Lit, String> {
        let word = self.word().to_string();
        if word == "f32_bits" || word == "f64_bits" {
            let (Some(_), bits, Some(_)) = (
                self.eat("(").then_some(()),
                self.word().to_string(),
                self.eat(")").then_some(()),
            ) else {
                return Err(format!("expected `{word}(bits)`"));
            };
            let bad = || format!("`{bits}` isn't a number of bits");
            return if word == "f32_bits" {
                bits.parse().map(Lit::F32).map_err(|_| bad())
            } else {
                bits.parse().map(Lit::F64).map_err(|_| bad())
            };
        }
        let bad = || format!("`{word}` isn't a literal like `5i32` or `1.5f64`");
        // An `i8` or `i16` can also be written unsigned, as in the text
        // format: `255i8` is `-1i8`.
        if let Some(num) = word.strip_suffix("i8") {
            let n = num
                .parse::<i8>()
                .or_else(|_| num.parse::<u8>().map(|n| n as i8));
            return n.map(Lit::I8).map_err(|_| bad());
        }
        if let Some(num) = word.strip_suffix("i16") {
            let n = num
                .parse::<i16>()
                .or_else(|_| num.parse::<u16>().map(|n| n as i16));
            return n.map(Lit::I16).map_err(|_| bad());
        }
        let (num, ty) = word.split_at(word.len().saturating_sub(3));
        match ty {
            "i32" => num.parse().map(Lit::I32).map_err(|_| bad()),
            "i64" => num.parse().map(Lit::I64).map_err(|_| bad()),
            "f32" => num.parse::<f32>().map(Lit::f32).map_err(|_| bad()),
            "f64" => num.parse::<f64>().map(Lit::f64).map_err(|_| bad()),
            _ => Err(bad()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_checks() {
        let text = "fn fn1() -> () {}\n\
                    //# run fn1(5i32, -2i64, 1.5f64) == 120i32\n\
                    // a plain comment\n\
                    //# run fn2() == ()\n\
                    //# run fn3(f32_bits(2143289344)) == (1i32, 2e3f32)\n\
                    //# run fn4(0i32) traps\n\
                    //# run fn5(255i8, -3i16) == 65535i16\n\
                    //# verify-error: isn't in scope\n\
                    //# wasm-skip: effect handlers\n\
                    //# wat\n\
                    //# opt\n\
                    //# opt: dce, copies\n";
        let kinds: Vec<CheckKind> = parse(text).unwrap().into_iter().map(|c| c.kind).collect();
        assert_eq!(
            kinds,
            [
                CheckKind::Run {
                    func: "fn1".into(),
                    args: vec![Lit::I32(5), Lit::I64(-2), Lit::f64(1.5)],
                    expect: Expect::Values(vec![Lit::I32(120)]),
                },
                CheckKind::Run {
                    func: "fn2".into(),
                    args: vec![],
                    expect: Expect::Values(vec![]),
                },
                CheckKind::Run {
                    func: "fn3".into(),
                    args: vec![Lit::F32(2143289344)],
                    expect: Expect::Values(vec![Lit::I32(1), Lit::f32(2000.0)]),
                },
                CheckKind::Run {
                    func: "fn4".into(),
                    args: vec![Lit::I32(0)],
                    expect: Expect::Trap,
                },
                CheckKind::Run {
                    func: "fn5".into(),
                    args: vec![Lit::I8(-1), Lit::I16(-3)],
                    expect: Expect::Values(vec![Lit::I16(-1)]),
                },
                CheckKind::VerifyError("isn't in scope".into()),
                CheckKind::WasmSkip("effect handlers".into()),
                CheckKind::Wat,
                CheckKind::Opt(None),
                CheckKind::Opt(Some(vec!["dce".into(), "copies".into()])),
            ]
        );
    }

    #[test]
    fn reports_bad_checks_with_their_line() {
        assert_eq!(parse("\n//# rnu fn1() == ()").unwrap_err().0, 2);
        assert!(
            parse("//# run fn1(5) == 1i32")
                .unwrap_err()
                .1
                .contains("`5`")
        );
        assert!(parse("//# run fn1() = 1i32").is_err());
        assert!(parse("//# wasm-error").is_err());
        assert!(parse("//# wat please").is_err());
        assert!(parse("//# opt: dce,").is_err());
        for (check, message) in [
            ("run () == ()", "expected a function like `fn1` or `sum`"),
            ("run fn1() == 1i32 2i32", "unexpected `2i32` at the end"),
            ("run fn1 == ()", "expected `(`"),
            ("run fn1(1i32 2i32) == ()", "expected `,` or `)`"),
            ("run fn1(f32_bits 5) == ()", "expected `f32_bits(bits)`"),
            ("run fn1(f64_bits(x)) == ()", "`x` isn't a number of bits"),
            (
                "run fn1() == 5x",
                "`5x` isn't a literal like `5i32` or `1.5f64`",
            ),
        ] {
            assert_eq!(
                parse(&format!("//# {check}")).unwrap_err().1,
                message,
                "for {check:?}"
            );
        }
    }
}
