//! Rows: the effects a function, closure or continuation may perform. See
//! "Effect types" in `docs/ir.md`.

use std::fmt;

use crate::id::EffectId;

/// A set of effects, or `Any`, which can perform anything.
///
/// The effects are kept sorted and without duplicates, so the derived
/// `Eq` and `Hash` are those of sets, and printing is deterministic.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Row {
    any: bool,
    effects: Vec<EffectId>,
}

impl Row {
    /// No effects.
    pub const NOTHING: Row = Row {
        any: false,
        effects: Vec::new(),
    };

    /// Anything: for frontends that don't track effects.
    pub const ANY: Row = Row {
        any: true,
        effects: Vec::new(),
    };

    /// These effects.
    pub fn new(effects: impl IntoIterator<Item = EffectId>) -> Row {
        let mut effects: Vec<EffectId> = effects.into_iter().collect();
        effects.sort();
        effects.dedup();
        Row {
            any: false,
            effects,
        }
    }

    /// Just `e`.
    pub fn one(e: EffectId) -> Row {
        Row {
            any: false,
            effects: vec![e],
        }
    }

    pub fn is_any(&self) -> bool {
        self.any
    }

    pub fn is_nothing(&self) -> bool {
        !self.any && self.effects.is_empty()
    }

    /// The effects, unless it's `Any`.
    pub fn effects(&self) -> &[EffectId] {
        &self.effects
    }

    /// Whether everything `self` may perform, `other` may too.
    pub fn is_in(&self, other: &Row) -> bool {
        other.any || (!self.any && self.effects.iter().all(|e| other.effects.contains(e)))
    }

    /// What either may perform.
    pub fn union(&self, other: &Row) -> Row {
        if self.any || other.any {
            return Row::ANY;
        }
        Row::new(self.effects.iter().chain(&other.effects).copied())
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
        if self.effects.is_empty() {
            return f.write_str("Nothing");
        }
        let names: Vec<String> = self.effects.iter().map(EffectId::to_string).collect();
        f.write_str(&names.join(" + "))
    }
}
