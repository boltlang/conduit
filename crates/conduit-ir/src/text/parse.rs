use std::collections::BTreeMap;

use super::Error;
use super::lex::{Pos, Tok, lex};
use super::names::{Names, Spelled, digits, spelled};
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
    for (j, (tok, _)) in toks.iter().enumerate() {
        match tok {
            Tok::Punct("{") => depth += 1,
            Tok::Punct("}") => depth -= 1,
            Tok::Ident(k) if depth == 0 => {
                if let (Some(i), Some((Tok::Ident(name), pos))) =
                    (KINDS.iter().position(|x| x == k), toks.get(j + 1))
                    && let Some(s) = spelled(name, k)
                {
                    found[i].push((s, *pos));
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

/// An attribute like `#[link(wasm, module = "env")]`.
struct Attr {
    name: String,
    pos: Pos,
    args: Vec<AttrArg>,
}

enum AttrArg {
    /// `wasm`
    Word(String),
    /// `module = "env"`
    Key(String, Box<str>),
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

    /// Attributes like `#[link(wasm, module = "env")]`, if there are any.
    fn attrs(&mut self) -> Res<Vec<Attr>> {
        let mut attrs = Vec::new();
        while self.eat_punct("#") {
            self.punct("[")?;
            let pos = self.pos();
            let name = self.ident("an attribute")?;
            let args = if self.is_punct("(") {
                self.list(|p| {
                    let word = p.ident("a word, or a key like `name = \"..\"`")?;
                    if p.eat_punct("=") {
                        Ok(AttrArg::Key(word, p.string()?))
                    } else {
                        Ok(AttrArg::Word(word))
                    }
                })?
            } else {
                Vec::new()
            };
            self.punct("]")?;
            attrs.push(Attr { name, pos, args });
        }
        Ok(attrs)
    }

    /// The attributes of a function's parameter: whether it's
    /// `#[borrow]`.
    fn param_attrs(&mut self) -> Res<bool> {
        let mut borrowed = false;
        for attr in self.attrs()? {
            match (attr.name.as_str(), &attr.args[..]) {
                ("borrow", []) => borrowed = true,
                ("borrow", _) => return self.error_at(attr.pos, "`borrow` takes no arguments"),
                _ => {
                    return self.error_at(
                        attr.pos,
                        format!(
                            "`{}` isn't an attribute of a function's parameter: only `borrow` is",
                            attr.name
                        ),
                    );
                }
            }
        }
        Ok(borrowed)
    }

    /// An extern's parameter: its attributes, its name if it has one, and
    /// its type.
    fn extern_param(&mut self) -> Res<(bool, Option<Box<str>>, Ty)> {
        let mut addr = false;
        for attr in self.attrs()? {
            match (attr.name.as_str(), &attr.args[..]) {
                ("addr", []) => addr = true,
                ("addr", _) => return self.error_at(attr.pos, "`addr` takes no arguments"),
                _ => {
                    return self.error_at(
                        attr.pos,
                        format!(
                            "`{}` isn't an attribute of a parameter: only `addr` is",
                            attr.name
                        ),
                    );
                }
            }
        }
        let name = match self.peek() {
            Tok::Var(s) => {
                let s = s.as_str().into();
                self.next();
                self.punct(":")?;
                Some(s)
            }
            _ => None,
        };
        Ok((addr, name, self.ty()?))
    }

    /// What an extern's attributes say: its `#[link(..)]`, which it must
    /// have one of, a `wasm` one with a `module` and a `name`, an `emu` one
    /// with a `name`, or a `c` one with a `symbol` and maybe a `lib`, the
    /// name or symbol being the extern's own by default; and its
    /// `#[abi(..)]`, by default `conduit`.
    fn link(&self, pos: Pos, attrs: Vec<Attr>, own: Option<&str>) -> Res<(Link, Abi)> {
        let mut link = None;
        let mut abi = None;
        for attr in attrs {
            if attr.name == "abi" {
                if abi.is_some() {
                    return self.error_at(attr.pos, "an extern has one `abi`");
                }
                abi = Some(match &attr.args[..] {
                    [AttrArg::Word(w)] if w == "flat" => Abi::Flat,
                    [AttrArg::Word(w)] if w == "conduit" => Abi::Conduit,
                    [AttrArg::Word(w)] => {
                        return self.error_at(
                            attr.pos,
                            format!("`{w}` isn't an ABI: `flat` and `conduit` are"),
                        );
                    }
                    _ => {
                        return self.error_at(attr.pos, "an `abi` is one word, like `abi(flat)`");
                    }
                });
                continue;
            }
            if attr.name != "link" {
                return self.error_at(
                    attr.pos,
                    format!(
                        "`{}` isn't an attribute of an extern: only `link` and `abi` are",
                        attr.name
                    ),
                );
            }
            if link.is_some() {
                return self.error_at(attr.pos, "an extern has one `link`");
            }
            let (kind, given) =
                self.kinded(attr.pos, attr.args, "link", "an extern", own, |kind| {
                    // The first of each kind's keys but a wasm link's `module`
                    // is the extern's own name if it's left out.
                    match kind {
                        "wasm" => (&["module", "name"], 1),
                        "emu" => (&["name"], 0),
                        _ => (&["symbol", "lib"], 0),
                    }
                })?;
            let mut given = given.into_iter();
            link = Some(match &*kind {
                "wasm" => {
                    let Some(module) = given.next().flatten() else {
                        return self.error_at(attr.pos, "a wasm `link` needs a `module`");
                    };
                    Link::Wasm {
                        module,
                        name: given.next().flatten().unwrap(),
                    }
                }
                "emu" => Link::Emu {
                    name: given.next().flatten().unwrap(),
                },
                _ => Link::C {
                    symbol: given.next().flatten().unwrap(),
                    lib: given.next().flatten(),
                },
            });
        }
        match link {
            Some(link) => Ok((link, abi.unwrap_or_default())),
            None => self.error_at(pos, "an extern needs a `#[link(..)]`, like `#[link(emu)]`"),
        }
    }

    /// The kind and keys of an attribute like `link` or `export`: a kind,
    /// `wasm`, `emu` or `c`, then keys, each once. `keys` gives a kind's
    /// keys and which of them is the item's own name, `own`, if it's left
    /// out. `what` names the attribute and `item` what it's on, for errors.
    fn kinded(
        &self,
        pos: Pos,
        args: Vec<AttrArg>,
        what: &str,
        item: &str,
        own: Option<&str>,
        keys: impl Fn(&str) -> (&'static [&'static str], usize),
    ) -> Res<(String, Vec<Option<Box<str>>>)> {
        let mut args = args.into_iter();
        let kind = match args.next() {
            Some(AttrArg::Word(kind)) if ["wasm", "emu", "c"].contains(&&*kind) => kind,
            Some(AttrArg::Word(kind)) => {
                return self.error_at(
                    pos,
                    format!("`{kind}` isn't a kind of {what}: `wasm`, `emu` and `c` are"),
                );
            }
            _ => {
                return self.error_at(pos, format!("a `{what}` starts with its kind, like `wasm`"));
            }
        };
        let a = if kind == "emu" { "an" } else { "a" };
        let (keys, named) = keys(&kind);
        let mut given: Vec<Option<Box<str>>> = vec![None; keys.len()];
        for arg in args {
            let slot = match &arg {
                AttrArg::Key(k, _) => keys.iter().position(|key| key == k),
                AttrArg::Word(_) => None,
            };
            match (slot, arg) {
                (Some(i), AttrArg::Key(_, v)) if given[i].is_none() => given[i] = Some(v),
                (_, AttrArg::Key(k, _) | AttrArg::Word(k)) => {
                    let keys: Vec<String> = keys.iter().map(|k| format!("`{k}`")).collect();
                    return self.error_at(
                        pos,
                        format!(
                            "{a} {kind} `{what}` takes {} once each, not `{k}`",
                            keys.join(" and ")
                        ),
                    );
                }
            }
        }
        if given[named].is_none() {
            given[named] = own.map(Into::into);
        }
        if given[named].is_none() {
            return self.error_at(
                pos,
                format!(
                    "{a} {kind} `{what}` of {item} without a name needs a `{}`",
                    keys[named]
                ),
            );
        }
        Ok((kind, given))
    }

    /// What a function's attributes say: its `#[export(..)]`, if it has
    /// one, a `wasm` or `emu` one with a `name`, or a `c` one with a
    /// `symbol`, by default the function's own.
    fn export(&self, attrs: Vec<Attr>, own: Option<&str>) -> Res<Option<Export>> {
        let mut export = None;
        for attr in attrs {
            if attr.name != "export" {
                return self.error_at(
                    attr.pos,
                    format!(
                        "`{}` isn't an attribute of a function: only `export` is",
                        attr.name
                    ),
                );
            }
            if export.is_some() {
                return self.error_at(attr.pos, "a function has one `export`");
            }
            let (kind, given) = self.kinded(
                attr.pos,
                attr.args,
                "export",
                "a function",
                own,
                |kind| match kind {
                    "c" => (&["symbol"], 0),
                    _ => (&["name"], 0),
                },
            )?;
            let name = given.into_iter().next().flatten().unwrap();
            export = Some(match &*kind {
                "wasm" => Export::Wasm { name },
                "emu" => Export::Emu { name },
                _ => Export::C { symbol: name },
            });
        }
        Ok(export)
    }

    fn module(&mut self) -> Res<Module> {
        let mut m = Module::default();
        loop {
            let attrs = self.attrs()?;
            let pos = self.pos();
            if !attrs.is_empty() && !self.is_keyword("extern") && !self.is_keyword("fn") {
                return self.error_at(
                    attrs[0].pos,
                    "only an extern or a function takes attributes, so far",
                );
            }
            let name = match self.peek() {
                Tok::Eof => return Ok(m),
                Tok::Ident(s) => s.clone(),
                _ => {
                    return self
                        .unexpected("`data`, `extern`, `const`, `effect`, `handler` or `fn`");
                }
            };
            self.next();
            let duplicate = match name.as_str() {
                "data" => {
                    let (id, debug_name) = self.def("data")?;
                    let decl = DataDecl {
                        debug_name,
                        ..self.data()?
                    };
                    let id = DataId(id);
                    m.data.insert(id, decl).map(|_| id.to_string())
                }
                "extern" => {
                    let (id, debug_name) = self.def("extern")?;
                    let ext = self.extern_decl(pos, attrs, debug_name)?;
                    let id = ExternId(id);
                    m.externs.insert(id, ext).map(|_| id.to_string())
                }
                "const" => {
                    let (id, debug_name) = self.def("const")?;
                    self.punct(":")?;
                    let ty = self.ty()?;
                    self.punct("=")?;
                    let value = self.const_value()?;
                    let decl = ConstDecl {
                        ty,
                        value,
                        debug_name,
                    };
                    let id = ConstId(id);
                    m.consts.insert(id, decl).map(|_| id.to_string())
                }
                "effect" => {
                    if self.is_keyword("throw") {
                        return self.error("`throw` is built in: no effect can be named that");
                    }
                    let (id, debug_name) = self.def("effect")?;
                    self.punct("{")?;
                    let mut ops = Vec::new();
                    while !self.eat_punct("}") {
                        ops.push(self.sig()?);
                    }
                    let id = EffectId(id);
                    let decl = EffectDecl { ops, debug_name };
                    m.effects.insert(id, decl).map(|_| id.to_string())
                }
                "handler" => {
                    let (id, debug_name) = self.def("handler")?;
                    let decl = HandlerDecl {
                        debug_name,
                        ..self.handler(pos)?
                    };
                    let id = HandlerId(id);
                    m.handlers.insert(id, decl).map(|_| id.to_string())
                }
                "fn" => {
                    let (id, name) = self.def("fn")?;
                    let id = FuncId(id);
                    if let Some(export) = self.export(attrs, name.as_deref())? {
                        m.exports.insert(id, export);
                    }
                    let mut f = self.func()?;
                    f.debug.name = name;
                    m.funcs.insert(id, f).map(|_| id.to_string())
                }
                _ => {
                    return self.error_at(
                        pos,
                        format!(
                            "expected `data`, `extern`, `const`, `effect`, `handler` or `fn`, \
                             found `{name}`"
                        ),
                    );
                }
            };
            if let Some(id) = duplicate {
                return self.error_at(pos, format!("{id} is defined more than once"));
            }
        }
    }

    /// An extern after its ID: its parameters, results and row. `attrs`
    /// are the attributes before it, at `pos`.
    fn extern_decl(
        &mut self,
        pos: Pos,
        attrs: Vec<Attr>,
        debug_name: Option<Box<str>>,
    ) -> Res<Extern> {
        let (link, abi) = self.link(pos, attrs, debug_name.as_deref())?;
        let params = self.list(Self::extern_param)?;
        self.punct("->")?;
        let results = self.list(Self::ty)?;
        let row = self.row()?;
        // Each list is empty if nothing is in it.
        let addr = if params.iter().any(|p| p.0) {
            params.iter().map(|p| p.0).collect()
        } else {
            Vec::new()
        };
        let param_names = if params.iter().any(|p| p.1.is_some()) {
            params.iter().map(|p| p.1.clone()).collect()
        } else {
            Vec::new()
        };
        let sig = Sig {
            params: params.into_iter().map(|p| p.2).collect(),
            results,
            row,
        };
        Ok(Extern {
            link,
            sig,
            abi,
            addr,
            param_names,
            debug_name,
        })
    }

    /// A handler after its ID, at `pos`, without its name.
    fn handler(&mut self, pos: Pos) -> Res<HandlerDecl> {
        let (mut_params, effect_params) = self.param_names()?;
        if let Some(m) = mut_params.first() {
            return self.error_at(
                pos,
                format!("a handler has no mutability parameters, but {m} is one"),
            );
        }
        self.effect_params = effect_params;
        let decl = self.handler_rest(pos);
        let effect_params = std::mem::take(&mut self.effect_params).len() as u32;
        Ok(HandlerDecl {
            effect_params,
            ..decl?
        })
    }

    /// A handler after its effect parameters.
    fn handler_rest(&mut self, pos: Pos) -> Res<HandlerDecl> {
        self.keyword("for")?;
        let mut effects = vec![EffectId(self.item("effect")?)];
        while self.eat_punct(",") {
            effects.push(EffectId(self.item("effect")?));
        }
        let row = self.row()?;
        self.keyword("env")?;
        let env = self.list(Self::ty)?;
        self.keyword("ops")?;
        // The clauses of each effect, the effects' apart by `;`.
        self.punct("(")?;
        let mut groups = vec![Vec::new()];
        loop {
            if !self.is_punct(";") && !self.is_punct(")") {
                groups.last_mut().unwrap().push(FuncId(self.item("fn")?));
            }
            if self.eat_punct(")") {
                break;
            }
            if self.eat_punct(";") {
                groups.push(Vec::new());
            } else if !self.eat_punct(",") {
                return self.unexpected("`,`, `;` or `)`");
            }
        }
        if groups.len() != effects.len() {
            return self.error_at(
                pos,
                format!(
                    "a handler for {} effects has clauses for {}, apart by `;`",
                    effects.len(),
                    groups.len()
                ),
            );
        }
        let ops = effects.into_iter().zip(groups).collect();
        self.keyword("ret")?;
        let ret = FuncId(self.item("fn")?);
        Ok(HandlerDecl {
            effect_params: 0,
            env,
            ops,
            ret,
            row,
            debug_name: None,
        })
    }

    fn data(&mut self) -> Res<DataDecl> {
        (self.params, self.effect_params) = self.param_names()?;
        self.punct("{")?;
        let ctors = self.ctors();
        let params = std::mem::take(&mut self.params).len() as u32;
        let effect_params = std::mem::take(&mut self.effect_params).len() as u32;
        Ok(DataDecl {
            params,
            effect_params,
            ctors: ctors?,
            debug_name: None,
        })
    }

    /// A data declaration's constructors, up to its `}`.
    fn ctors(&mut self) -> Res<Vec<CtorDecl>> {
        let mut ctors = Vec::new();
        while !self.eat_punct("}") {
            ctors.push(CtorDecl {
                fields: self.list(Self::ty)?,
            });
        }
        Ok(ctors)
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
            "host" => Ty::Host,
            "nullable" => {
                self.keyword("host")?;
                Ty::NullableHost
            }
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

    /// `[ty]`, as in `box[i32]`.
    fn bracketed_ty(&mut self) -> Res<Ty> {
        self.punct("[")?;
        let ty = self.ty()?;
        self.punct("]")?;
        Ok(ty)
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

    /// A row: `Effect20 + E1`, `Nothing` or `Any`, where `throw` is the
    /// built-in effect. A name is an effect parameter if the declaration
    /// being read has one of that name, and an effect otherwise.
    fn row_items(&mut self) -> Res<Row> {
        if self.eat_keyword("Nothing") {
            return Ok(Row::NOTHING);
        }
        if self.eat_keyword("Any") {
            return Ok(Row::ANY);
        }
        let (mut effects, mut params, mut throw) = (Vec::new(), Vec::new(), false);
        loop {
            if self.eat_keyword("throw") {
                throw = true;
                if !self.eat_punct("+") {
                    break;
                }
                continue;
            }
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
        Ok(Row::with_params(effects, params).with_throw(throw))
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
        let params = self.list(|p| {
            let borrowed = p.param_attrs()?;
            Ok((p.binder(&mut info)?, borrowed))
        })?;
        // Empty if no parameter is borrowed.
        let borrowed = if params.iter().any(|p| p.1) {
            params.iter().map(|p| p.1).collect()
        } else {
            Vec::new()
        };
        let params = params.into_iter().map(|p| p.0).collect();
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
            borrowed,
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
        crate::deep(|| {
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
        })
    }

    /// What ends a block.
    fn tail(&mut self, info: &mut FuncInfo) -> Res<Tail> {
        crate::deep(|| {
            let pos = self.pos();
            let name = self.ident("`let`, `join`, `jump`, `case`, `unreachable` or `trap`")?;
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
                    format!(
                        "expected `let`, `join`, `jump`, `case`, `unreachable` or `trap`, found `{name}`"
                    ),
                ),
            }
        })
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
    /// object `List.1(v, ..)`, a closure `closure fn3`, a box
    /// `box[i32](0i32)`, or another constant.
    fn const_value(&mut self) -> Res<ConstValue> {
        match self.peek() {
            Tok::Ident(s) if s == "closure" => {
                self.next();
                Ok(ConstValue::Closure(FuncId(self.item("fn")?)))
            }
            Tok::Ident(s)
                if s == "box"
                    && matches!(self.toks.get(self.i + 1), Some((Tok::Punct("["), _))) =>
            {
                self.next();
                let ty = self.bracketed_ty()?;
                self.punct("(")?;
                let inner = self.const_value()?;
                self.punct(")")?;
                Ok(ConstValue::Box(ty, Box::new(inner)))
            }
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
            "forget" => Op::Forget,
            "host_null" => Op::HostNull,
            "host_is_null" => Op::HostIsNull,
            "host_unwrap" => Op::HostUnwrap,
            "host_function" => Op::HostFunction,
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
            "array_load" => Op::ArrayLoad,
            "array_store" => Op::ArrayStore,
            "array_len" => Op::ArrayLen,
            "array_clone" => Op::ArrayClone,
            "ref_new" => Op::RefNew,
            "ref_get" => Op::RefGet,
            "ref_set" => Op::RefSet,
            "box" => Op::Box(self.bracketed_ty()?),
            "unbox" => Op::Unbox(self.bracketed_ty()?),
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
                Some(s) => Some(Prim::Convert(self.bracketed_ty()?, s)),
                None => None,
            },
            "reinterpret" => match sign {
                None => Some(Prim::Reinterpret(self.bracketed_ty()?)),
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
