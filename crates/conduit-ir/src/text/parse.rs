use std::collections::BTreeMap;

use super::Error;
use super::lex::{Pos, Tok, lex};
use crate::*;

/// Reads a module from its text.
pub fn parse(src: &str) -> Result<Module, Error> {
    let mut p = Parser {
        toks: lex(src)?,
        i: 0,
    };
    p.module()
}

type Res<T> = Result<T, Error>;

struct Parser {
    toks: Vec<(Tok, Pos)>,
    i: usize,
}

/// What a function's body says about its variables and labels.
#[derive(Default)]
struct FuncInfo {
    vars: BTreeMap<u32, Ty>,
    max_label: u32,
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.i].0
    }

    fn pos(&self) -> Pos {
        self.toks[self.i].1
    }

    fn next(&mut self) -> Tok {
        let tok = self.toks[self.i].0.clone();
        if self.i + 1 < self.toks.len() {
            self.i += 1;
        }
        tok
    }

    fn error_at<T>(&self, pos: Pos, message: impl Into<String>) -> Res<T> {
        Err(Error {
            line: pos.line,
            col: pos.col,
            message: message.into(),
        })
    }

    fn error<T>(&self, message: impl Into<String>) -> Res<T> {
        self.error_at(self.pos(), message)
    }

    fn unexpected<T>(&self, expected: &str) -> Res<T> {
        let found = match self.peek() {
            Tok::Ident(s) => format!("`{s}`"),
            Tok::Var(n) => format!("`%{n}`"),
            Tok::Num(s) => format!("`{s}`"),
            Tok::Str(s) => format!("{s:?}"),
            Tok::Punct(p) => format!("`{p}`"),
            Tok::Eof => "the end of the text".to_string(),
        };
        self.error(format!("expected {expected}, found {found}"))
    }

    fn is_punct(&self, p: &str) -> bool {
        matches!(self.peek(), Tok::Punct(q) if *q == p)
    }

    fn is_keyword(&self, kw: &str) -> bool {
        matches!(self.peek(), Tok::Ident(s) if s == kw)
    }

    fn eat_punct(&mut self, p: &str) -> bool {
        let found = self.is_punct(p);
        if found {
            self.next();
        }
        found
    }

    fn eat_keyword(&mut self, kw: &str) -> bool {
        let found = self.is_keyword(kw);
        if found {
            self.next();
        }
        found
    }

    fn punct(&mut self, p: &str) -> Res<()> {
        if self.eat_punct(p) {
            Ok(())
        } else {
            self.unexpected(&format!("`{p}`"))
        }
    }

    fn keyword(&mut self, kw: &str) -> Res<()> {
        if self.eat_keyword(kw) {
            Ok(())
        } else {
            self.unexpected(&format!("`{kw}`"))
        }
    }

    fn ident(&mut self, expected: &str) -> Res<String> {
        match self.peek() {
            Tok::Ident(s) => {
                let s = s.clone();
                self.next();
                Ok(s)
            }
            _ => self.unexpected(expected),
        }
    }

    /// An ID like `fn1`: `prefix` followed by a number.
    fn id(&mut self, prefix: &str) -> Res<u64> {
        let pos = self.pos();
        let what = format!("an ID like `{prefix}1`");
        let name = self.ident(&what)?;
        match name.strip_prefix(prefix).and_then(digits) {
            Some(n) => Ok(n),
            None => self.error_at(pos, format!("expected {what}, found `{name}`")),
        }
    }

    /// A plain number, like a field index or a label count.
    fn index(&mut self) -> Res<u32> {
        match self.peek() {
            Tok::Num(s) => {
                let n = s
                    .parse()
                    .or_else(|_| self.error(format!("`{s}` isn't an index")))?;
                self.next();
                Ok(n)
            }
            _ => self.unexpected("a number"),
        }
    }

    fn string(&mut self) -> Res<Box<str>> {
        match self.peek() {
            Tok::Str(s) => {
                let s = s.as_str().into();
                self.next();
                Ok(s)
            }
            _ => self.unexpected("a string"),
        }
    }

    /// `( item, .. )`
    fn list<T>(&mut self, mut item: impl FnMut(&mut Self) -> Res<T>) -> Res<Vec<T>> {
        self.punct("(")?;
        let mut items = Vec::new();
        if self.eat_punct(")") {
            return Ok(items);
        }
        loop {
            items.push(item(self)?);
            if self.eat_punct(")") {
                return Ok(items);
            }
            self.punct(",")?;
        }
    }

    fn module(&mut self) -> Res<Module> {
        let mut m = Module::default();
        loop {
            let pos = self.pos();
            let name = match self.peek() {
                Tok::Eof => return Ok(m),
                Tok::Ident(s) => s.clone(),
                _ => return self.unexpected("`data`, `extern`, `effect`, `handler` or `fn`"),
            };
            self.next();
            let duplicate =
                match name.as_str() {
                    "data" => {
                        let id = DataId(self.id("data")?);
                        let decl = self.data()?;
                        m.data.insert(id, decl).map(|_| id.to_string())
                    }
                    "extern" => {
                        let id = ExternId(self.id("extern")?);
                        let (module, name) = (self.string()?, self.string()?);
                        let sig = self.sig()?;
                        m.externs
                            .insert(id, Extern { module, name, sig })
                            .map(|_| id.to_string())
                    }
                    "effect" => {
                        let id = EffectId(self.id("effect")?);
                        self.punct("{")?;
                        let mut ops = Vec::new();
                        while !self.eat_punct("}") {
                            ops.push(self.sig()?);
                        }
                        m.effects
                            .insert(id, EffectDecl { ops })
                            .map(|_| id.to_string())
                    }
                    "handler" => {
                        let id = HandlerId(self.id("handler")?);
                        self.keyword("for")?;
                        let effect = EffectId(self.id("effect")?);
                        self.keyword("env")?;
                        let env = self.list(Self::ty)?;
                        self.keyword("ops")?;
                        let ops = self.list(|p| p.id("fn").map(FuncId))?;
                        self.keyword("ret")?;
                        let ret = FuncId(self.id("fn")?);
                        m.handlers
                            .insert(
                                id,
                                HandlerDecl {
                                    effect,
                                    env,
                                    ops,
                                    ret,
                                },
                            )
                            .map(|_| id.to_string())
                    }
                    "fn" => {
                        let id = FuncId(self.id("fn")?);
                        let f = self.func()?;
                        m.funcs.insert(id, f).map(|_| id.to_string())
                    }
                    _ => return self.error_at(
                        pos,
                        format!(
                            "expected `data`, `extern`, `effect`, `handler` or `fn`, found `{name}`"
                        ),
                    ),
                };
            if let Some(id) = duplicate {
                return self.error_at(pos, format!("{id} is defined more than once"));
            }
        }
    }

    fn data(&mut self) -> Res<DataDecl> {
        self.punct("{")?;
        let mut ctors = Vec::new();
        while !self.eat_punct("}") {
            ctors.push(CtorDecl {
                fields: self.list(Self::ty)?,
            });
        }
        Ok(DataDecl { ctors })
    }

    fn mutability(&mut self) -> Res<Mutability> {
        let m = match self.peek() {
            Tok::Ident(s) if s == "imm" => Mutability::Imm,
            Tok::Ident(s) if s == "mut" => Mutability::Mut,
            Tok::Ident(s) if s == "ro" => Mutability::ReadOnly,
            Tok::Ident(s) if s == "inherit" => Mutability::Inherit,
            _ => return self.unexpected("`imm`, `mut`, `ro` or `inherit`"),
        };
        self.next();
        Ok(m)
    }

    fn ty(&mut self) -> Res<Ty> {
        let pos = self.pos();
        let name = self.ident("a type")?;
        Ok(match name.as_str() {
            "i32" => Ty::I32,
            "i64" => Ty::I64,
            "f32" => Ty::F32,
            "f64" => Ty::F64,
            "cont" => Ty::Cont(Box::new(self.sig()?)),
            "ref" => Ty::Ref(Box::new(self.ty()?)),
            "imm" | "mut" | "ro" | "inherit" => {
                self.i -= 1;
                let m = self.mutability()?;
                if self.eat_punct("[") {
                    let elem = self.ty()?;
                    self.punct("]")?;
                    Ty::Array(Box::new(elem), m)
                } else if self.eat_keyword("fn") {
                    Ty::Fn(Box::new(self.sig()?), m)
                } else if self.eat_keyword("any") {
                    Ty::Any(m)
                } else {
                    Ty::Data(DataId(self.id("data")?), m)
                }
            }
            _ => return self.error_at(pos, format!("expected a type, found `{name}`")),
        })
    }

    fn sig(&mut self) -> Res<Sig> {
        let params = self.list(Self::ty)?;
        self.punct("->")?;
        let results = self.list(Self::ty)?;
        Ok(Sig { params, results })
    }

    fn var(&mut self) -> Res<Var> {
        match self.peek() {
            Tok::Var(n) => {
                let n = *n;
                self.next();
                Ok(Var(n))
            }
            _ => self.unexpected("a variable"),
        }
    }

    /// `%3: i32`, which gives `%3` its type.
    fn binder(&mut self, info: &mut FuncInfo) -> Res<Var> {
        let pos = self.pos();
        let v = self.var()?;
        self.punct(":")?;
        let ty = self.ty()?;
        match info.vars.get(&v.0) {
            Some(old) if *old != ty => {
                self.error_at(pos, format!("{v} has type {old} elsewhere, not {ty}"))
            }
            _ => {
                info.vars.insert(v.0, ty);
                Ok(v)
            }
        }
    }

    fn label(&mut self) -> Res<Label> {
        let pos = self.pos();
        let name = self.ident("a label like `j1` or `ret`")?;
        if name == "ret" {
            return Ok(Label::RET);
        }
        match name
            .strip_prefix('j')
            .and_then(digits)
            .and_then(|n| u32::try_from(n).ok())
        {
            Some(n) if n > 0 => Ok(Label(n)),
            _ => self.error_at(
                pos,
                format!("expected a label like `j1` or `ret`, found `{name}`"),
            ),
        }
    }

    fn func(&mut self) -> Res<Func> {
        let mut info = FuncInfo::default();
        let params = self.list(|p| p.binder(&mut info))?;
        self.punct("->")?;
        let results = self.list(Self::ty)?;
        let labels = if self.eat_keyword("labels") {
            Some(self.index()?)
        } else {
            None
        };
        if self.eat_keyword("vars") {
            self.list(|p| p.binder(&mut info))?;
        }
        self.punct("{")?;
        let body = self.block(&mut info)?;
        self.punct("}")?;

        let len = info.vars.last_key_value().map_or(0, |(&n, _)| n + 1);
        let mut vars = Vec::with_capacity(len as usize);
        for n in 0..len {
            match info.vars.remove(&n) {
                Some(ty) => vars.push(ty),
                None => return self.error(format!("%{n} has no type: give it one in `vars(..)`")),
            }
        }
        Ok(Func {
            vars,
            labels: labels.unwrap_or(info.max_label + 1),
            params,
            results,
            body,
        })
    }

    fn source(&mut self) -> Res<Option<SourceId>> {
        if self.eat_punct("@") {
            Ok(Some(SourceId(self.id("src")?)))
        } else {
            Ok(None)
        }
    }

    fn block(&mut self, info: &mut FuncInfo) -> Res<Block> {
        let mut lets = Vec::new();
        while self.eat_keyword("let") {
            let mut binds = Vec::new();
            if !self.is_punct("=") {
                loop {
                    binds.push(self.binder(info)?);
                    if !self.eat_punct(",") {
                        break;
                    }
                }
            }
            self.punct("=")?;
            let op = self.op()?;
            let args = self.list(Self::atom)?;
            let source = self.source()?;
            lets.push(Let {
                binds,
                op,
                args,
                source,
            });
        }
        let tail = self.tail(info)?;
        Ok(Block { lets, tail })
    }

    /// What ends a block.
    fn tail(&mut self, info: &mut FuncInfo) -> Res<Tail> {
        let pos = self.pos();
        let name = self.ident("`let`, `join`, `jump`, `case` or `unreachable`")?;
        match name.as_str() {
            "join" => {
                let rec = self.eat_keyword("rec");
                let mut defs = Vec::new();
                loop {
                    let label_pos = self.pos();
                    let label = self.label()?;
                    if label == Label::RET {
                        return self.error_at(label_pos, "`ret` can't be defined");
                    }
                    info.max_label = info.max_label.max(label.0);
                    let params = self.list(|p| p.binder(info))?;
                    self.punct("=")?;
                    let body = self.block(info)?;
                    defs.push(JoinDef {
                        label,
                        params,
                        body,
                    });
                    if !self.eat_keyword("and") {
                        break;
                    }
                }
                self.keyword("in")?;
                let body = Box::new(self.block(info)?);
                Ok(Tail::Join { rec, defs, body })
            }
            "jump" => {
                let target = self.label()?;
                let args = self.list(Self::atom)?;
                let source = self.source()?;
                Ok(Tail::Jump {
                    target,
                    args,
                    source,
                })
            }
            "case" => {
                let scrut = self.atom()?;
                self.punct("{")?;
                let (mut arms, mut default) = (Vec::new(), None);
                while !self.eat_punct("}") {
                    let tag_pos = self.pos();
                    let tag = if self.eat_keyword("_") {
                        None
                    } else if let Tok::Num(s) = self.peek() {
                        let n = s
                            .parse()
                            .or_else(|_| self.error(format!("`{s}` isn't a tag")))?;
                        self.next();
                        Some(Tag::Int(n))
                    } else {
                        Some(Tag::Ctor(self.ctor()?))
                    };
                    self.punct("=>")?;
                    let body = self.block(info)?;
                    match tag {
                        Some(tag) => arms.push(Arm { tag, body }),
                        None if default.is_some() => {
                            return self.error_at(tag_pos, "a case has one `_` arm at most");
                        }
                        None => default = Some(Box::new(body)),
                    }
                }
                Ok(Tail::Case {
                    scrut,
                    arms,
                    default,
                })
            }
            "unreachable" => Ok(Tail::Unreachable),
            _ => self.error_at(
                pos,
                format!("expected `let`, `join`, `jump`, `case` or `unreachable`, found `{name}`"),
            ),
        }
    }

    /// `data10.1`
    fn ctor(&mut self) -> Res<CtorId> {
        let data = DataId(self.id("data")?);
        self.punct(".")?;
        Ok(CtorId {
            data,
            index: self.index()?,
        })
    }

    fn atom(&mut self) -> Res<Atom> {
        let pos = self.pos();
        if !matches!(self.peek(), Tok::Var(_) | Tok::Num(_) | Tok::Ident(_)) {
            return self.unexpected("a variable or a literal");
        }
        match self.next() {
            Tok::Var(n) => Ok(Atom::Var(Var(n))),
            Tok::Num(s) => {
                let lit = parse_lit(&s).ok_or(());
                lit.map(Atom::Lit).or_else(|()| {
                    self.error_at(
                        pos,
                        format!("`{s}` isn't a literal: it needs a type, like `{s}i32`"),
                    )
                })
            }
            Tok::Ident(name) if name == "f32_bits" || name == "f64_bits" => {
                self.punct("(")?;
                let bits = match self.next() {
                    Tok::Num(s) => s.parse::<u64>().ok(),
                    _ => None,
                };
                self.punct(")")?;
                match (name.as_str(), bits) {
                    ("f32_bits", Some(b)) if b <= u32::MAX.into() => {
                        Ok(Atom::Lit(Lit::F32(b as u32)))
                    }
                    ("f64_bits", Some(b)) => Ok(Atom::Lit(Lit::F64(b))),
                    _ => self.error_at(pos, format!("bad bits for `{name}`")),
                }
            }
            Tok::Ident(name) => self.error_at(
                pos,
                format!("expected a variable or a literal, found `{name}`"),
            ),
            _ => self.error_at(pos, "expected a variable or a literal"),
        }
    }

    fn op(&mut self) -> Res<Op> {
        let pos = self.pos();
        let name = self.ident("an op")?;
        let in_brackets = |p: &mut Self| -> Res<Ty> {
            p.punct("[")?;
            let ty = p.ty()?;
            p.punct("]")?;
            Ok(ty)
        };
        let op = match name.as_str() {
            "call" => Op::Call(FuncId(self.id("fn")?)),
            "call_extern" => Op::CallExtern(ExternId(self.id("extern")?)),
            "closure" => Op::Closure(FuncId(self.id("fn")?)),
            "apply" => Op::Apply,
            "new" => Op::New(self.ctor()?),
            "get" | "set" => {
                let c = self.ctor()?;
                self.punct(".")?;
                let i = self.index()?;
                if name == "get" {
                    Op::Get(c, i)
                } else {
                    Op::Set(c, i)
                }
            }
            "array_new" => Op::ArrayNew,
            "array_fill" => Op::ArrayFill,
            "array_get" => Op::ArrayGet,
            "array_set" => Op::ArraySet,
            "array_len" => Op::ArrayLen,
            "ref_new" => Op::RefNew,
            "ref_get" => Op::RefGet,
            "ref_set" => Op::RefSet,
            "box" => Op::Box(in_brackets(self)?),
            "unbox" => Op::Unbox(in_brackets(self)?),
            "copy" => {
                self.punct("[")?;
                let m = self.mutability()?;
                self.punct("]")?;
                Op::Copy(m)
            }
            "handle" => Op::Handle(HandlerId(self.id("handler")?)),
            "perform" => {
                let e = EffectId(self.id("effect")?);
                self.punct(".")?;
                Op::Perform(e, self.index()?)
            }
            "resume" => Op::Resume,
            _ => Op::Prim(self.prim(pos, &name)?),
        };
        Ok(op)
    }

    fn prim(&mut self, pos: Pos, name: &str) -> Res<Prim> {
        let mut sign = None;
        if self.eat_punct(".") {
            let sign_pos = self.pos();
            sign = Some(match self.ident("`s` or `u`")?.as_str() {
                "s" => Sign::Signed,
                "u" => Sign::Unsigned,
                other => {
                    return self
                        .error_at(sign_pos, format!("expected `s` or `u`, found `{other}`"));
                }
            });
        }
        let signed = |make: fn(Sign) -> Prim| sign.map(make);
        let plain = |p: Prim| if sign.is_none() { Some(p) } else { None };
        let prim = match name {
            "add" => plain(Prim::Add),
            "sub" => plain(Prim::Sub),
            "mul" => plain(Prim::Mul),
            "div" => signed(Prim::Div),
            "rem" => signed(Prim::Rem),
            "neg" => plain(Prim::Neg),
            "and" => plain(Prim::And),
            "or" => plain(Prim::Or),
            "xor" => plain(Prim::Xor),
            "shl" => plain(Prim::Shl),
            "shr" => signed(Prim::Shr),
            "eq" => plain(Prim::Eq),
            "ne" => plain(Prim::Ne),
            "lt" => signed(Prim::Lt),
            "le" => signed(Prim::Le),
            "gt" => signed(Prim::Gt),
            "ge" => signed(Prim::Ge),
            "abs" => plain(Prim::Abs),
            "sqrt" => plain(Prim::Sqrt),
            "min" => plain(Prim::Min),
            "max" => plain(Prim::Max),
            "convert" => match sign {
                Some(s) => {
                    self.punct("[")?;
                    let ty = self.ty()?;
                    self.punct("]")?;
                    Some(Prim::Convert(ty, s))
                }
                None => None,
            },
            "reinterpret" => match sign {
                None => {
                    self.punct("[")?;
                    let ty = self.ty()?;
                    self.punct("]")?;
                    Some(Prim::Reinterpret(ty))
                }
                Some(_) => None,
            },
            _ => return self.error_at(pos, format!("unknown op `{name}`")),
        };
        match prim {
            Some(prim) => Ok(prim),
            None if sign.is_some() => {
                self.error_at(pos, format!("`{name}` has no signed or unsigned form"))
            }
            None => self.error_at(pos, format!("`{name}` needs `.s` or `.u`, like `{name}.s`")),
        }
    }
}

/// `n` as a decimal number without a sign.
fn digits(n: &str) -> Option<u64> {
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse().ok()
}

/// A literal with its type suffix, like `5i32` or `1.5f64`.
fn parse_lit(s: &str) -> Option<Lit> {
    let (number, suffix) = s.split_at(s.len().checked_sub(3)?);
    match suffix {
        "i32" => number.parse().ok().map(Lit::I32),
        "i64" => number.parse().ok().map(Lit::I64),
        "f32" => number.parse::<f32>().ok().map(Lit::f32),
        "f64" => number.parse::<f64>().ok().map(Lit::f64),
        _ => None,
    }
}
