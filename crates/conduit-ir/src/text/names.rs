//! How numbered things are spelled in the text: as a number like `fn3` or
//! `%3`, as a name like `sum` or `%acc`, or as both, like `sum#3`.
//!
//! A name without a number gets one: the smallest of its kind that no
//! other is written with, in the order they're met. For items, that's the
//! order they're defined in; for variables and labels, the order they first
//! appear in their function. The parser numbers them this way, and the
//! printer writes `name#3` wherever reading the name alone would give
//! another number, and for every name that more than one thing has.

use std::collections::{BTreeMap, BTreeSet, HashMap};

/// What an identifier says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Spelled {
    /// `fn3`, or `%3`: just a number.
    Number(u64),
    /// `sum`, or `sum#3`.
    Named(String, Option<u64>),
}

/// Reads `text` as a thing whose numbered form is `prefix` and a number.
pub(super) fn spelled(text: &str, prefix: &str) -> Option<Spelled> {
    if let Some((name, n)) = text.split_once('#') {
        let n = digits(n)?;
        return valid(name, prefix).then(|| Spelled::Named(name.to_string(), Some(n)));
    }
    if let Some(n) = text.strip_prefix(prefix).and_then(digits) {
        return Some(Spelled::Number(n));
    }
    valid(text, prefix).then(|| Spelled::Named(text.to_string(), None))
}

fn digits(n: &str) -> Option<u64> {
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse().ok()
}

/// Whether `name` can be written as a name of something numbered with
/// `prefix`: an identifier, which isn't a keyword where a name can be, and
/// doesn't read as a number.
pub(super) fn valid(name: &str, prefix: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let ident = (first.is_ascii_alphabetic()
        || first == '_'
        || (prefix.is_empty() && first.is_ascii_digit()))
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    let number = name.strip_prefix(prefix).and_then(digits).is_some();
    // `E1` in a row is an effect parameter, so no effect is named that.
    let param = prefix == "effect" && name.strip_prefix('E').and_then(digits).is_some();
    ident && !number && !param && name != "_" && (prefix.is_empty() || !KEYWORDS.contains(&name))
}

/// Words that mean something where a name of an item or label can be.
const KEYWORDS: &[&str] = &[
    "Any", "Nothing", "and", "any", "cont", "does", "env", "f32", "f32_bits", "f64", "f64_bits",
    "fn", "for", "i16", "i32", "i64", "i8", "imm", "in", "inherit", "labels", "mut", "ops", "rec",
    "ref", "ret", "reuse", "ro", "vars", "where",
];

/// The names of one kind of thing, as the parser reads them.
#[derive(Default)]
pub(super) struct Names {
    /// For each name, the numbers it's written with, as in `sum#3`.
    numbered: HashMap<String, Vec<u64>>,
    /// The numbers names without one got.
    given: HashMap<String, u64>,
}

impl Names {
    /// Numbers `found`, in the order they're met, starting from `start`.
    /// With `distinct`, two of them without a number are two things, which
    /// is an error for items; otherwise they're one, as for variables.
    pub(super) fn new(found: &[Spelled], start: u64, distinct: bool) -> Result<Names, String> {
        let mut taken = BTreeSet::new();
        let mut names = Names::default();
        for s in found {
            match s {
                Spelled::Number(n) => {
                    taken.insert(*n);
                }
                Spelled::Named(name, Some(n)) => {
                    taken.insert(*n);
                    let ns = names.numbered.entry(name.clone()).or_default();
                    if !ns.contains(n) {
                        ns.push(*n);
                    }
                }
                Spelled::Named(_, None) => {}
            }
        }
        let mut next = start;
        for s in found {
            let Spelled::Named(name, None) = s else {
                continue;
            };
            if names.given.contains_key(name) {
                if distinct {
                    return Err(format!(
                        "`{name}` is defined more than once: give each a number, like `{name}#1`"
                    ));
                }
                continue;
            }
            if !distinct && names.numbered.contains_key(name) {
                continue;
            }
            while taken.contains(&next) {
                next += 1;
            }
            taken.insert(next);
            names.given.insert(name.clone(), next);
        }
        Ok(names)
    }

    /// The number a use of `s` means.
    pub(super) fn get(&self, s: &Spelled) -> Result<u64, String> {
        match s {
            Spelled::Number(n) | Spelled::Named(_, Some(n)) => Ok(*n),
            Spelled::Named(name, None) => {
                let mut all: Vec<u64> = self.numbered.get(name).cloned().unwrap_or_default();
                all.extend(self.given.get(name));
                match all[..] {
                    [n] => Ok(n),
                    [] => Err(format!("`{name}` isn't defined")),
                    _ => Err(format!(
                        "`{name}` is the name of more than one: say which, like `{name}#{}`",
                        all[0]
                    )),
                }
            }
        }
    }

    /// The number and name a definition of `s` gives.
    pub(super) fn def(&self, s: &Spelled) -> (u64, Option<Box<str>>) {
        match s {
            Spelled::Number(n) => (*n, None),
            Spelled::Named(name, Some(n)) => (*n, Some(name.as_str().into())),
            Spelled::Named(name, None) => (self.given[name], Some(name.as_str().into())),
        }
    }
}

/// How to write each of some numbered things. `found` has each number
/// once, with its name, in the order the parser will meet them.
pub(super) fn spellings<'a>(
    prefix: &str,
    start: u64,
    found: &[(u64, Option<&'a str>)],
) -> BTreeMap<u64, String> {
    let name = |n: Option<&'a str>| n.filter(|n| valid(n, prefix));
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for &(_, n) in found {
        if let Some(n) = name(n) {
            *counts.entry(n).or_default() += 1;
        }
    }
    // Those written with their number: those without a name, those that
    // share theirs, and those a name alone would give another number.
    let mut numbered: BTreeSet<u64> = found
        .iter()
        .filter(|&&(_, n)| name(n).is_none_or(|n| counts[n] > 1))
        .map(|&(id, _)| id)
        .collect();
    loop {
        let mut taken = numbered.clone();
        let mut next = start;
        let mut wrong = None;
        for &(id, _) in found.iter().filter(|(id, _)| !numbered.contains(id)) {
            while taken.contains(&next) {
                next += 1;
            }
            if next != id {
                wrong = Some(id);
                break;
            }
            taken.insert(next);
        }
        match wrong {
            Some(id) => {
                numbered.insert(id);
            }
            None => break,
        }
    }
    found
        .iter()
        .map(|&(id, n)| {
            let text = match name(n) {
                None => format!("{prefix}{id}"),
                Some(n) if numbered.contains(&id) => format!("{n}#{id}"),
                Some(n) => n.to_string(),
            };
            (id, text)
        })
        .collect()
}
