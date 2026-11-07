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
    /// A `trap` was reached.
    Trap,
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
    /// A host function ended the program with an exit code, as a process's
    /// `exit` does: see [`HostError::Exit`].
    Exit(i32),
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
            ErrorKind::Trap => write!(f, "trap"),
            ErrorKind::ContinuationResumedTwice => {
                write!(f, "a continuation was resumed more than once")
            }
            ErrorKind::Unhandled(effect) => write!(f, "no handler for {effect}"),
            ErrorKind::StackOverflow => write!(f, "stack overflow"),
            ErrorKind::OutOfFuel => write!(f, "out of fuel"),
            ErrorKind::WriteToImm => write!(f, "write to an imm object"),
            ErrorKind::Undefined(message) => write!(f, "undefined behavior: {message}"),
            ErrorKind::Host(message) => write!(f, "host function failed: {message}"),
            ErrorKind::Exit(code) => write!(f, "exited with code {code}"),
            ErrorKind::Unverified(message) => write!(f, "invalid IR: {message}"),
        }
    }
}

impl std::error::Error for Error {}

/// Why a host function gives no results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostError {
    /// It failed, which ends the run with [`ErrorKind::Host`].
    Failed(String),
    /// It ends the program with this exit code, unwinding no handlers, which
    /// ends the run with [`ErrorKind::Exit`].
    Exit(i32),
}

impl From<String> for HostError {
    fn from(message: String) -> HostError {
        HostError::Failed(message)
    }
}

impl From<&str> for HostError {
    fn from(message: &str) -> HostError {
        HostError::Failed(message.into())
    }
}

impl From<HostError> for ErrorKind {
    fn from(e: HostError) -> ErrorKind {
        match e {
            HostError::Failed(message) => ErrorKind::Host(message),
            HostError::Exit(code) => ErrorKind::Exit(code),
        }
    }
}

pub(crate) fn unverified(message: impl Into<String>) -> ErrorKind {
    ErrorKind::Unverified(message.into())
}
