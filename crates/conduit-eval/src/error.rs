use std::fmt;

use conduit_ir::{EffectId, FuncId, SourceId};

/// Why evaluation stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    /// The function that was running, if any.
    pub func: Option<FuncId>,
    /// The source of the last `let` or `jump` that ran.
    pub source: Option<SourceId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    /// Division of the smallest integer by -1.
    IntegerOverflow,
    /// A float converted to an integer that is NaN or out of its range.
    InvalidConversion,
    /// An array index, or a length for `ArrayFill`, that's out of range.
    OutOfBounds { index: u64, len: u64 },
    /// `unreachable` was reached.
    Unreachable,
    /// A continuation was resumed after it had been resumed already.
    ContinuationResumedTwice,
    /// `perform` with no enclosing handler of its effect.
    Unhandled(EffectId),
    /// More frames than [`Machine::max_frames`](crate::Machine::max_frames).
    StackOverflow,
    /// The program ran more steps than its fuel allows.
    OutOfFuel,
    /// A write to an object that is `Imm`. Verified IR can only do this if
    /// an optimization broke the mutability rules.
    WriteToImm,
    /// Undefined behavior of the IR.
    Undefined(String),
    /// A host function failed.
    Host(String),
    /// IR that the verifier would have rejected.
    Unverified(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(func) = self.func {
            write!(f, "{func}")?;
            if let Some(source) = self.source {
                write!(f, " ({source})")?;
            }
            write!(f, ": ")?;
        }
        match &self.kind {
            ErrorKind::IntegerOverflow => write!(f, "integer overflow in division"),
            ErrorKind::InvalidConversion => {
                write!(f, "a float converted to an integer is NaN or out of range")
            }
            ErrorKind::OutOfBounds { index, len } => {
                write!(f, "index {index} is out of bounds for length {len}")
            }
            ErrorKind::Unreachable => write!(f, "reached unreachable code"),
            ErrorKind::ContinuationResumedTwice => {
                write!(f, "a continuation was resumed more than once")
            }
            ErrorKind::Unhandled(effect) => write!(f, "no handler for {effect}"),
            ErrorKind::StackOverflow => write!(f, "stack overflow"),
            ErrorKind::OutOfFuel => write!(f, "out of fuel"),
            ErrorKind::WriteToImm => write!(f, "write to an imm object"),
            ErrorKind::Undefined(message) => write!(f, "undefined behavior: {message}"),
            ErrorKind::Host(message) => write!(f, "host function failed: {message}"),
            ErrorKind::Unverified(message) => write!(f, "invalid IR: {message}"),
        }
    }
}

impl std::error::Error for Error {}

pub(crate) fn unverified(message: impl Into<String>) -> ErrorKind {
    ErrorKind::Unverified(message.into())
}
