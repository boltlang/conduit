use std::collections::BTreeMap;

use super::Error;
use super::lex::{Pos, Tok, lex};
use super::names::{Names, Spelled, spelled};
use crate::*;

/// Reads a module from its text.
pub fn parse(src: &str) -> Result<Module, Error> {
    let toks = lex(src)?;
    let items = item_names(&toks)?;
    let mut p = Parser {
        toks,
        i: 0,
        params: Vec::new(),
        effect_params: Vec::new(),
        items,
        vars: Names::default(),
        labels: Names::default(),
    };
    p.module()
}

/// The kinds of item, by the keyword that defines one and the prefix of
/// their numbered form.
const KINDS: [&str; 6] = ["data", "extern", "effect", "handler", "fn", "const"];

fn kind(prefix: &str) -> usize {
    KINDS
        .iter()
        .position(|k| *k == prefix)
        .expect("a kind of item")
}

/// The names of the items `toks` defines, of each kind in [`KINDS`].
fn item_names(toks: &[(Tok, Pos)]) -> Result<Vec<Names>, Error> {
    let mut found: Vec<Vec<(Spelled, Pos)>> = vec![Vec::new(); KINDS.len()];
    let mut depth = 0;
    for w in toks.windows(2) {
        match &w[0].0 {
            Tok::Punct("{") => depth += 1,
            Tok::Punct("}") => depth -= 1,
            Tok::Ident(k) if depth == 0 => {
                if let (Some(i), Tok::Ident(name)) = (KINDS.iter().position(|x| x == k), &w[1].0)
                    && let Some(s) = spelled(name, k)
                {
                    found[i].push((s, w[1].1));
                }
            }
            _ => {}
        }
    }
    found
        .iter()
        .map(|defs| {
            let spellings: Vec<Spelled> = defs.iter().map(|(s, _)| s.clone()).collect();
            Names::new(&spellings, 0, true).map_err(|message| Error {
                line: defs[0].1.line,
                col: defs[0].1.col,
                message,
            })
        })
        .collect()
}

type Res<T> = Result<T, Error>;

struct Parser {
    toks: Vec<(Tok, Pos)>,
    i: usize,
    /// The mutability parameters of the data declaration or function
    /// being read, by name: `Param(i + 1)` is `params[i]`.
    params: Vec<String>,
    /// The effect parameters of the data declaration, function or handler
    /// being read, by name: `E{i + 1}` is `effect_params[i]`.
    effect_params: Vec<String>,
    /// The names of items, of each kind in [`KINDS`].
    items: Vec<Names>,
    /// The names of the variables and labels of the function being read.
    vars: Names,
    labels: Names,
}

/// What a function's body says about its variables and labels.
#[derive(Default)]
struct FuncInfo {
    vars: BTreeMap<u32, Ty>,
    max_label: u32,
    debug: FuncDebug,
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
            Tok::Var(s) => format!("`%{s}`"),
            Tok::Num(s) => format!("`{s}`"),
            Tok::Str(s) => format!("{s:?}"),
            Tok::Bytes(_) => "a byte string".to_string(),
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

    /// An ID like `src1`: `prefix` followed by a number.
    fn id(&mut self, prefix: &str) -> Res<u64> {
        let pos = self.pos();
        let what = format!("an ID like `{prefix}1`");
        let name = self.ident(&what)?;
        match name.strip_prefix(prefix).and_then(digits) {
            Some(n) => Ok(n),
            None => self.error_at(pos, format!("expected {what}, found `{name}`")),
        }
    }

    /// How the next identifier spells an item of the kind `prefix`.
    fn spelled(&mut self, prefix: &str) -> Res<Spelled> {
        let pos = self.pos();
        let what = format!("an ID like `{prefix}1`, or a name");
        let name = self.ident(&what)?;
        match spelled(&name, prefix) {
            Some(s) => Ok(s),
            None => self.error_at(pos, format!("expected {what}, found `{name}`")),
        }
    }

    /// A use of an item of the kind `prefix`: `fn1`, `sum` or `sum#1`.
    fn item(&mut self, prefix: &str) -> Res<u64> {
        let pos = self.pos();
        let s = self.spelled(prefix)?;
        match self.items[kind(prefix)].get(&s) {
            Ok(n) => Ok(n),
            Err(message) => self.error_at(pos, message),
        }
    }

    /// The definition of an item of the kind `prefix`: its number, and its
    /// name if it has one.
    fn def(&mut self, prefix: &str) -> Res<(u64, Option<Box<str>>)> {
        let s = self.spelled(prefix)?;
        Ok(self.items[kind(prefix)].def(&s))
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
            if !self.eat_punct(",") {
                return self.unexpected("`,` or `)`");
            }
        }
    }

    fn module(&mut self) -> Res<Module> {
        let mut m = Module::default();
        loop {
            let pos = self.pos();
            let name = match self.peek() {
                Tok::Eof => return Ok(m),
                Tok::Ident(s) => s.clone(),
                _ => {
                    return self
                        .unexpected("`data`, `extern`, `const`, `effect`, `handler` or `fn`");
                }
            };
            self.next();
            let duplicate =
                match name.as_str() {
                    "data" => {
                        let (id, debug_name) = self.def("data")?;
                        let id = DataId(id);
                        let decl = DataDecl {
                            debug_name,
                            ..self.data()?
                        };
                        m.data.insert(id, decl).map(|_| id.to_string())
                    }
                    "extern" => {
                        let (id, debug_name) = self.def("extern")?;
                        let id = ExternId(id);
                        let (module, name) = (self.string()?, self.string()?);
                        let sig = self.sig()?;
                        let ext = Extern {
                            module,
                            name,
                            sig,
                            debug_name,
                        };
                        m.externs.insert(id, ext).map(|_| id.to_string())
                    }
                    "const" => {
                        let (id, debug_name) = self.def("const")?;
                        let id = ConstId(id);
                        self.punct(":")?;
                        let ty = self.ty()?;
                        self.punct("=")?;
                        let value = self.const_value()?;
                        let decl = ConstDecl {
                            ty,
                            value,
                            debug_name,
                        };
                        m.consts.insert(id, decl).map(|_| id.to_string())
                    }
                    "effect" => {
                        let (id, debug_name) = self.def("effect")?;
                        let id = EffectId(id);
                        self.punct("{")?;
                        let mut ops = Vec::new();
                        while !self.eat_punct("}") {
                            ops.push(self.sig()?);
                        }
                        m.effects
                            .insert(id, EffectDecl { ops, debug_name })
                            .map(|_| id.to_string())
                    }
                    "handler" => {
                        let (id, debug_name) = self.def("handler")?;
                        let id = HandlerId(id);
                        let (mut_params, effect_params) = self.param_names()?;
                        if let Some(m) = mut_params.first() {
                            return self.error_at(
                                pos,
                                format!("a handler has no mutability parameters, but {m} is one"),
                            );
                        }
                        self.effect_params = effect_params;
                        self.keyword("for")?;
                        let effect = EffectId(self.item("effect")?);
                        let row = self.row()?;
                        self.keyword("env")?;
                        let env = self.list(Self::ty)?;
                        self.keyword("ops")?;
                        let ops = self.list(|p| p.item("fn").map(FuncId))?;
                        self.keyword("ret")?;
                        let ret = FuncId(self.item("fn")?);
                        let effect_params =
                            std::mem::take(&mut self.effect_params).len() as u32;
                        m.handlers
                            .insert(
                                id,
                                HandlerDecl {
                                    effect,
                                    effect_params,
                                    env,
                                    ops,
                                    ret,
                                    row,
                                    debug_name,
                                },
                            )
                            .map(|_| id.to_string())
                    }
                    "fn" => {
                        let (id, name) = self.def("fn")?;
                        let id = FuncId(id);
                        let mut f = self.func()?;
                        f.debug.name = name;
                        m.funcs.insert(id, f).map(|_| id.to_string())
                    }
                    _ => return self.error_at(
                        pos,
                        format!(
                            "expected `data`, `extern`, `const`, `effect`, `handler` or `fn`, found `{name}`"
                        ),
                    ),
                };
            if let Some(id) = duplicate {
                return self.error_at(pos, format!("{id} is defined more than once"));
            }
        }
    }

    fn data(&mut self) -> Res<DataDecl> {
        (self.params, self.effect_params) = self.param_names()?;
        self.punct("{")?;
        let mut ctors = Vec::new();
        let result = loop {
            if self.eat_punct("}") {
                break Ok(());
            }
            match self.list(Self::ty) {
                Ok(fields) => ctors.push(CtorDecl { fields }),
                Err(e) => break Err(e),
            }
        };
        let params = std::mem::take(&mut self.params).len() as u32;
        let effect_params = std::mem::take(&mut self.effect_params).len() as u32;
        result?;
        Ok(DataDecl {
            params,
            effect_params,
            ctors,
            debug_name: None,
        })
    }

    /// The mutability and effect parameters a data declaration, function
    /// or handler declares, like `['m1, 'm2, E1]`, if it has any: names
    /// with a quote are mutability parameters, and names starting with an
    /// uppercase letter are effect parameters, which come after.
    fn param_names(&mut self) -> Res<(Vec<String>, Vec<String>)> {
        let (mut params, mut effects) = (Vec::new(), Vec::new());
        if !self.eat_punct("[") {
            return Ok((params, effects));
        }
        loop {
            let pos = self.pos();
            let name = self.ident("a parameter like `'m1` or `E1`")?;
            let is_effect = name.starts_with(|c: char| c.is_ascii_uppercase());
            if !name.starts_with('\'') && !is_effect {
                return self.error_at(
                    pos,
                    format!("expected a parameter like `'m1` or `E1`, found `{name}`"),
                );
            }
            if is_effect && matches!(name.as_str(), "Nothing" | "Any") {
                return self.error_at(pos, format!("`{name}` can't be an effect parameter"));
            }
            if params.contains(&name) || effects.contains(&name) {
                return self.error_at(pos, format!("`{name}` is declared more than once"));
            }
            if is_effect {
                effects.push(name);
            } else if !effects.is_empty() {
                return self.error_at(
                    pos,
                    format!("mutability parameter `{name}` comes after an effect parameter"),
                );
            } else {
                params.push(name);
            }
            if self.eat_punct("]") {
                return Ok((params, effects));
            }
            self.punct(",")?;
        }
    }

    /// Mutability arguments and rows in brackets, like `[imm, 'm1, Log]`,
    /// if there are any. The mutabilities come first.
    fn margs(&mut self) -> Res<(Vec<Mutability>, Vec<Row>)> {
        let (mut margs, mut rows) = (Vec::new(), Vec::new());
        if self.eat_punct("[") {
            loop {
                if self.is_mutability() {
                    if !rows.is_empty() {
                        return self.error("a mutability argument comes after a row");
                    }
                    margs.push(self.mutability()?);
                } else {
                    rows.push(self.row_items()?);
                }
                if self.eat_punct("]") {
                    break;
                }
                self.punct(",")?;
            }
        }
        Ok((margs, rows))
    }

    /// Whether the next token is a mutability.
    fn is_mutability(&self) -> bool {
        matches!(self.peek(), Tok::Ident(s) if matches!(s.as_str(), "imm" | "mut" | "ro" | "inherit") || s.starts_with('\''))
    }

    fn mutability(&mut self) -> Res<Mutability> {
        let m = match self.peek() {
            Tok::Ident(s) if s == "imm" => Mutability::Imm,
            Tok::Ident(s) if s == "mut" => Mutability::Mut,
            Tok::Ident(s) if s == "ro" => Mutability::ReadOnly,
            Tok::Ident(s) if s == "inherit" => Mutability::Inherit,
            Tok::Ident(s) if s.starts_with('\'') => match self.params.iter().position(|p| p == s) {
                Some(i) => Mutability::Param(i as u32 + 1),
                None => {
                    let s = s.clone();
                    return self.error(format!(
                        "unknown mutability parameter `{s}`: declare it in brackets after the \
                         data type's or function's ID"
                    ));
                }
            },
            _ => return self.unexpected("`imm`, `mut`, `ro` or `inherit`"),
        };
        self.next();
        Ok(m)
    }

    fn ty(&mut self) -> Res<Ty> {
        let pos = self.pos();
        let name = self.ident("a type")?;
        Ok(match name.as_str() {
            "i8" => Ty::I8,
            "i16" => Ty::I16,
            "i32" => Ty::I32,
            "i64" => Ty::I64,
            "f32" => Ty::F32,
            "f64" => Ty::F64,
            "cont" => Ty::Cont(Box::new(self.sig()?)),
            "ref" => Ty::Ref(Box::new(self.ty()?)),
            "reuse" => Ty::Reuse(self.ctor()?),
            m if matches!(m, "imm" | "mut" | "ro" | "inherit") || m.starts_with('\'') => {
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
                    let d = DataId(self.item("data")?);
                    let (args, rows) = self.margs()?;
                    Ty::Data(d, m, args, rows)
                }
            }
            _ => return self.error_at(pos, format!("expected a type, found `{name}`")),
        })
    }

    fn sig(&mut self) -> Res<Sig> {
        let params = self.list(Self::ty)?;
        self.punct("->")?;
        let results = self.list(Self::ty)?;
        let row = self.row()?;
        Ok(Sig {
            params,
            results,
            row,
        })
    }

    /// `does Effect20 + E1`, `does Nothing` or `does Any`, or nothing for
    /// no effects.
    fn row(&mut self) -> Res<Row> {
        if !self.eat_keyword("does") {
            return Ok(Row::NOTHING);
        }
        self.row_items()
    }

    /// A row: `Effect20 + E1`, `Nothing` or `Any`. A name is an effect
    /// parameter if the declaration being read has one of that name, and
    /// an effect otherwise.
    fn row_items(&mut self) -> Res<Row> {
        if self.eat_keyword("Nothing") {
            return Ok(Row::NOTHING);
        }
        if self.eat_keyword("Any") {
            return Ok(Row::ANY);
        }
        let (mut effects, mut params) = (Vec::new(), Vec::new());
        loop {
            let param = match self.peek() {
                Tok::Ident(s) => self.effect_params.iter().position(|p| p == s),
                _ => None,
            };
            match param {
                Some(i) => {
                    self.next();
                    params.push(i as u32 + 1);
                }
                None => effects.push(EffectId(self.item("effect")?)),
            }
            if !self.eat_punct("+") {
                break;
            }
        }
        Ok(Row::with_params(effects, params))
    }

    fn var(&mut self, info: &mut FuncInfo) -> Res<Var> {
        let pos = self.pos();
        let Tok::Var(text) = self.peek() else {
            return self.unexpected("a variable");
        };
        let Some(s) = spelled(text, "") else {
            let text = text.clone();
            return self.error_at(pos, format!("`%{text}` isn't a variable"));
        };
        self.next();
        let n = match self.vars.get(&s) {
            Ok(n) => n,
            Err(message) => return self.error_at(pos, format!("%{message}")),
        };
        let Ok(n) = u32::try_from(n) else {
            return self.error_at(pos, "too many variables");
        };
        if let Spelled::Named(name, _) = s {
            info.debug.vars.insert(Var(n), name.into());
        }
        Ok(Var(n))
    }

    /// `%3: i32`, which gives `%3` its type.
    fn binder(&mut self, info: &mut FuncInfo) -> Res<Var> {
        let pos = self.pos();
        let v = self.var(info)?;
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

    fn label(&mut self, info: &mut FuncInfo) -> Res<Label> {
        let pos = self.pos();
        let name = self.ident("a label like `j1`, `loop` or `ret`")?;
        if name == "ret" {
            return Ok(Label::RET);
        }
        let Some(s) = spelled(&name, "j") else {
            return self.error_at(
                pos,
                format!("expected a label like `j1`, `loop` or `ret`, found `{name}`"),
            );
        };
        let n = match self.labels.get(&s) {
            Ok(n) => n,
            Err(message) => return self.error_at(pos, message),
        };
        match u32::try_from(n) {
            Ok(n) if n > 0 => {
                if let Spelled::Named(name, _) = s {
                    info.debug.labels.insert(Label(n), name.into());
                }
                Ok(Label(n))
            }
            _ => self.error_at(pos, format!("`{name}` isn't a label: they start at `j1`")),
        }
    }

    /// Numbers the variables and labels of the function that starts at the
    /// next token, by a look ahead to the end of its body.
    fn func_names(&mut self) -> Res<()> {
        let (mut vars, mut labels) = (Vec::new(), Vec::new());
        let mut depth = 0;
        let mut j = self.i;
        let label = |tok: &Tok| match tok {
            Tok::Ident(s) if s != "ret" => spelled(s, "j"),
            _ => None,
        };
        while j < self.toks.len() {
            match &self.toks[j].0 {
                Tok::Punct("{") => depth += 1,
                Tok::Punct("}") => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                Tok::Var(text) => vars.extend(spelled(text, "")),
                Tok::Ident(k) if k == "join" || k == "and" || k == "jump" => {
                    let mut next = j + 1;
                    if k == "join" && matches!(&self.toks[next].0, Tok::Ident(r) if r == "rec") {
                        next += 1;
                    }
                    labels.extend(label(&self.toks[next].0));
                }
                Tok::Eof => break,
                _ => {}
            }
            j += 1;
        }
        let pos = self.pos();
        self.vars = Names::new(&vars, 0, false).or_else(|m| self.error_at(pos, m))?;
        self.labels = Names::new(&labels, 1, false).or_else(|m| self.error_at(pos, m))?;
        Ok(())
    }

    fn func(&mut self) -> Res<Func> {
        self.func_names()?;
        (self.params, self.effect_params) = self.param_names()?;
        let f = self.func_rest();
        self.params.clear();
        self.effect_params.clear();
        f
    }

    /// A function after its mutability parameters.
    fn func_rest(&mut self) -> Res<Func> {
        let mut info = FuncInfo::default();
        let params = self.list(|p| p.binder(&mut info))?;
        self.punct("->")?;
        let results = self.list(Self::ty)?;
        let row = self.row()?;
        let mut assumes = Vec::new();
        if self.eat_keyword("where") {
            loop {
                let a = self.mutability()?;
                self.punct(":")?;
                let b = self.mutability()?;
                assumes.push((a, b));
                if !self.eat_punct(",") {
                    break;
                }
            }
        }
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
            mut_params: MutParams {
                count: self.params.len() as u32,
                assumes,
            },
            effect_params: self.effect_params.len() as u32,
            vars,
            labels: labels.unwrap_or(info.max_label + 1),
            params,
            results,
            row,
            body,
            debug: info.debug,
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
            let args = self.list(|p| p.atom(info))?;
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
                    let label = self.label(info)?;
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
                let target = self.label(info)?;
                let args = self.list(|p| p.atom(info))?;
                let source = self.source()?;
                Ok(Tail::Jump {
                    target,
                    args,
                    source,
                })
            }
            "case" => {
                let scrut = self.atom(info)?;
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
            "trap" => Ok(Tail::Trap),
            _ => self.error_at(
                pos,
                format!("expected `let`, `join`, `jump`, `case` or `unreachable`, found `{name}`"),
            ),
        }
    }

    /// `data10.1`
    fn ctor(&mut self) -> Res<CtorId> {
        let data = DataId(self.item("data")?);
        self.punct(".")?;
        Ok(CtorId {
            data,
            index: self.index()?,
        })
    }

    fn atom(&mut self, info: &mut FuncInfo) -> Res<Atom> {
        let pos = self.pos();
        if !matches!(self.peek(), Tok::Var(_) | Tok::Num(_) | Tok::Ident(_)) {
            return self.unexpected("a variable or a literal");
        }
        if let Tok::Var(_) = self.peek() {
            return self.var(info).map(Atom::Var);
        }
        if let Tok::Ident(name) = self.peek()
            && name != "f32_bits"
            && name != "f64_bits"
        {
            let name = name.clone();
            return self.error_at(
                pos,
                format!("expected a variable or a literal, found `{name}`"),
            );
        }
        self.lit().map(Atom::Lit)
    }

    /// A literal: `5i32`, `1.5f64` or `f32_bits(2143289344)`.
    fn lit(&mut self) -> Res<Lit> {
        let pos = self.pos();
        match self.next() {
            Tok::Num(s) => parse_lit(&s).ok_or(()).or_else(|()| {
                self.error_at(
                    pos,
                    format!("`{s}` isn't a literal: it needs a type, like `{s}i32`"),
                )
            }),
            Tok::Ident(name) if name == "f32_bits" || name == "f64_bits" => {
                self.punct("(")?;
                let bits = match self.next() {
                    Tok::Num(s) => s.parse::<u64>().ok(),
                    _ => None,
                };
                self.punct(")")?;
                match (name.as_str(), bits) {
                    ("f32_bits", Some(b)) if b <= u32::MAX.into() => Ok(Lit::F32(b as u32)),
                    ("f64_bits", Some(b)) => Ok(Lit::F64(b)),
                    _ => self.error_at(pos, format!("bad bits for `{name}`")),
                }
            }
            _ => self.error_at(pos, "expected a literal"),
        }
    }

    /// A constant's value: a literal, an array `[v, ..]` or `b"hi"`, an
    /// object `List.1(v, ..)`, or another constant.
    fn const_value(&mut self) -> Res<ConstValue> {
        match self.peek() {
            Tok::Num(_) => self.lit().map(ConstValue::Lit),
            Tok::Bytes(bytes) => {
                let elems = bytes
                    .iter()
                    .map(|&b| ConstValue::Lit(Lit::I8(b as i8)))
                    .collect();
                self.next();
                Ok(ConstValue::Array(elems))
            }
            Tok::Ident(s) if s == "f32_bits" || s == "f64_bits" => self.lit().map(ConstValue::Lit),
            Tok::Punct("[") => {
                self.next();
                let mut elems = Vec::new();
                if self.eat_punct("]") {
                    return Ok(ConstValue::Array(elems));
                }
                loop {
                    elems.push(self.const_value()?);
                    if self.eat_punct("]") {
                        return Ok(ConstValue::Array(elems));
                    }
                    if !self.eat_punct(",") {
                        return self.unexpected("`,` or `]`");
                    }
                }
            }
            Tok::Ident(_) if matches!(self.toks.get(self.i + 1), Some((Tok::Punct("."), _))) => {
                let c = self.ctor()?;
                let fields = self.list(Self::const_value)?;
                Ok(ConstValue::New(c, fields))
            }
            Tok::Ident(_) => Ok(ConstValue::Const(ConstId(self.item("const")?))),
            _ => self.unexpected("a literal, an array, a constructor or a constant"),
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
            "call" => {
                let f = FuncId(self.item("fn")?);
                let (margs, rows) = self.margs()?;
                Op::Call(f, margs, rows)
            }
            "call_extern" => Op::CallExtern(ExternId(self.item("extern")?)),
            "closure" => {
                let f = FuncId(self.item("fn")?);
                let (margs, rows) = self.margs()?;
                Op::Closure(f, margs, rows)
            }
            "apply" => Op::Apply,
            "new" => Op::New(self.ctor()?),
            "const" => Op::Const(ConstId(self.item("const")?)),
            "undefined" => Op::Undefined,
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
            "array_alloc" => Op::ArrayAlloc,
            "array_copy" => Op::ArrayCopy,
            "array_get" => Op::ArrayGet,
            "array_set" => Op::ArraySet,
            "array_len" => Op::ArrayLen,
            "array_clone" => Op::ArrayClone,
            "ref_new" => Op::RefNew,
            "ref_get" => Op::RefGet,
            "ref_set" => Op::RefSet,
            "box" => Op::Box(in_brackets(self)?),
            "unbox" => Op::Unbox(in_brackets(self)?),
            "array_freeze" => Op::ArrayFreeze,
            "handle" => {
                let h = HandlerId(self.item("handler")?);
                let (margs, rows) = self.margs()?;
                if !margs.is_empty() {
                    return self.error_at(pos, "a handler takes rows, not mutabilities");
                }
                Op::Handle(h, rows)
            }
            "perform" => {
                let e = EffectId(self.item("effect")?);
                self.punct(".")?;
                Op::Perform(e, self.index()?)
            }
            "resume" => Op::Resume,
            "dup" => Op::Dup,
            "drop" => Op::Drop,
            "is_unique" => Op::IsUnique,
            "free" => Op::Free,
            "decref" => Op::DecRef,
            "drop_reuse" => Op::DropReuse,
            "reuse_token" => Op::ReuseToken,
            "reuse" => Op::Reuse(self.ctor()?),
            "discontinue" => {
                let e = EffectId(self.item("effect")?);
                self.punct(".")?;
                Op::Discontinue(e, self.index()?)
            }
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

/// A literal with its type suffix, like `5i32` or `1.5f64`. An `i8` or
/// `i16` can also be written unsigned: `255i8` is `-1i8`.
fn parse_lit(s: &str) -> Option<Lit> {
    if let Some(number) = s.strip_suffix("i8") {
        let n = number.parse::<i8>().ok();
        return n
            .or_else(|| number.parse::<u8>().ok().map(|n| n as i8))
            .map(Lit::I8);
    }
    if let Some(number) = s.strip_suffix("i16") {
        let n = number.parse::<i16>().ok();
        return n
            .or_else(|| number.parse::<u16>().ok().map(|n| n as i16))
            .map(Lit::I16);
    }
    let (number, suffix) = s.split_at(s.len().checked_sub(3)?);
    match suffix {
        "i32" => number.parse().ok().map(Lit::I32),
        "i64" => number.parse().ok().map(Lit::I64),
        "f32" => number.parse::<f32>().ok().map(Lit::f32),
        "f64" => number.parse::<f64>().ok().map(Lit::f64),
        _ => None,
    }
}
