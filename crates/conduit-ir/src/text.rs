//! A text format for the IR: [`parse`] reads it, and [`print`] writes it.
//!
//! Printing then parsing gives back the same module, and parsing then
//! printing gives back the same text if it was printed by [`print`]. The
//! format is described in `docs/ir/text.md`. A small
//! example:
//!
//! ```text
//! data data10 {
//!   (i32, i32)
//! }
//!
//! fn fn1(%0: mut data10) -> (i32) {
//!   let %1: i32 = get data10.0.0(%0)
//!   let %2: i32 = add(%1, 1i32)
//!   jump ret(%2)
//! }
//! ```

mod lex;
mod names;
mod parse;
pub(crate) mod print;

use std::fmt;

use crate::{Func, FuncId, Module};

pub use parse::parse;
pub(crate) use print::atom as atom_text;

/// The text of a module.
pub fn print(m: &Module) -> String {
    print::module(m)
}

/// The text of one function, as it appears in a module.
pub fn print_func(id: FuncId, f: &Func) -> String {
    print::func(id, f)
}

/// The function of `m` that `name` means where a function is used in the
/// text: `fn3`, `sum` or `sum#3`.
pub fn find_func(m: &Module, name: &str) -> Result<FuncId, String> {
    let found = |id: FuncId| {
        if m.funcs.contains_key(&id) {
            Ok(id)
        } else {
            Err(format!("there's no function `{name}`"))
        }
    };
    match names::spelled(name, "fn") {
        Some(names::Spelled::Number(n) | names::Spelled::Named(_, Some(n))) => found(FuncId(n)),
        Some(names::Spelled::Named(_, None)) => {
            let ids: Vec<FuncId> = m
                .funcs
                .iter()
                .filter(|(_, f)| f.debug.name.as_deref() == Some(name))
                .map(|(&id, _)| id)
                .collect();
            match ids[..] {
                [id] => Ok(id),
                [] => Err(format!("there's no function `{name}`")),
                _ => Err(format!(
                    "more than one function is `{name}`: say which, like `{name}#{}`",
                    ids[0].0
                )),
            }
        }
        None => Err(format!(
            "`{name}` isn't a function: expected `fn1` or a name"
        )),
    }
}

impl fmt::Display for Module {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&print(self))
    }
}

/// Where text that doesn't parse goes wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// 1-based.
    pub line: u32,
    /// 1-based, in characters.
    pub col: u32,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.message)
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests;
