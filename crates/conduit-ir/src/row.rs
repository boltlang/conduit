//! Rows: the effects a function, closure or continuation may perform. See
//! "Effect types" in `docs/ir.md`.

use std::fmt;

use crate::id::EffectId;

/// A set of effects and effect parameters, or `Any`, which can perform
/// anything.
///
/// Parameter `i` is `E{i}`, counted from 1, of the function, data type or
/// handler the row is in, as mutability parameters are. Calls, closures,
/// data types and `handle` give a row for each: see [`Row::subst`].
///
/// The effects and parameters are kept sorted and without duplicates, so
/// the derived `Eq` and `Hash` are those of sets, and printing is
/// deterministic.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Row {
    any: bool,
    effects: Vec<EffectId>,
    params: Vec<u32>,
}

impl Row {
    /// No effects.
    pub const NOTHING: Row = Row {
        any: false,
        effects: Vec::new(),
        params: Vec::new(),
    };

    /// Anything: for frontends that don't track effects.
    pub const ANY: Row = Row {
        any: true,
        effects: Vec::new(),
        params: Vec::new(),
    };

    /// These effects.
    pub fn new(effects: impl IntoIterator<Item = EffectId>) -> Row {
        let mut effects: Vec<EffectId> = effects.into_iter().collect();
        effects.sort();
        effects.dedup();
        Row {
            any: false,
            effects,
            params: Vec::new(),
        }
    }

    /// Just `e`.
    pub fn one(e: EffectId) -> Row {
        Row {
            any: false,
            effects: vec![e],
            params: Vec::new(),
        }
    }

    /// Just effect parameter `i`, counted from 1.
    pub fn param(i: u32) -> Row {
        Row {
            any: false,
            effects: Vec::new(),
            params: vec![i],
        }
    }

    /// These effects and effect parameters.
    pub fn with_params(
        effects: impl IntoIterator<Item = EffectId>,
        params: impl IntoIterator<Item = u32>,
    ) -> Row {
        let mut row = Row::new(effects);
        row.params = params.into_iter().collect();
        row.params.sort();
        row.params.dedup();
        row
    }

    pub fn is_any(&self) -> bool {
        self.any
    }

    pub fn is_nothing(&self) -> bool {
        !self.any && self.effects.is_empty() && self.params.is_empty()
    }

    /// The effects, unless it's `Any`.
    pub fn effects(&self) -> &[EffectId] {
        &self.effects
    }

    /// The effect parameters, counted from 1, unless it's `Any`.
    pub fn params(&self) -> &[u32] {
        &self.params
    }

    /// Whether everything `self` may perform, `other` may too. A parameter
    /// is only in itself: it may be any row.
    pub fn is_in(&self, other: &Row) -> bool {
        other.any
            || (!self.any
                && self.effects.iter().all(|e| other.effects.contains(e))
                && self.params.iter().all(|p| other.params.contains(p)))
    }

    /// What either may perform.
    pub fn union(&self, other: &Row) -> Row {
        if self.any || other.any {
            return Row::ANY;
        }
        Row::with_params(
            self.effects.iter().chain(&other.effects).copied(),
            self.params.iter().chain(&other.params).copied(),
        )
    }

    /// The row with each parameter `i` replaced by `args[i - 1]`. A
    /// parameter without an argument stays, and one given `Any` makes the
    /// row `Any`.
    pub fn subst(&self, args: &[Row]) -> Row {
        if self.any || self.params.is_empty() || args.is_empty() {
            return self.clone();
        }
        let mut row = Row::new(self.effects.iter().copied());
        for &p in &self.params {
            match args.get(p as usize - 1) {
                Some(arg) => row = row.union(arg),
                None => row = row.union(&Row::param(p)),
            }
        }
        row
    }

    /// `self` without `e`. `Any` stays `Any`.
    pub fn without(&self, e: EffectId) -> Row {
        let mut row = self.clone();
        row.effects.retain(|&x| x != e);
        row
    }
}

impl fmt::Display for Row {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.any {
            return f.write_str("Any");
        }
        if self.effects.is_empty() && self.params.is_empty() {
            return f.write_str("Nothing");
        }
        let names: Vec<String> = self
            .effects
            .iter()
            .map(EffectId::to_string)
            .chain(self.params.iter().map(|p| format!("E{p}")))
            .collect();
        f.write_str(&names.join(" + "))
    }
}
