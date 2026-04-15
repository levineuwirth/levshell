//! Crate-wide error type for the context engine.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, ContextError>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ContextError {
    /// The expression DSL failed to parse. `position` is the byte offset in
    /// the source string where the first token couldn't be consumed.
    #[error("expression parse error at position {position}: {message}")]
    Parse { message: String, position: usize },

    /// Evaluating the expression hit a runtime type mismatch (e.g. comparing
    /// a string to a number with `<`).
    #[error("expression evaluation error: {0}")]
    Eval(String),

    /// A rule referenced a signal that isn't present in the context. Not
    /// fatal on its own — the evaluator treats missing signals as `false` in
    /// boolean position — but callers may want to surface it.
    #[error("unknown signal: {0}")]
    UnknownSignal(String),
}

impl ContextError {
    pub fn parse(message: impl Into<String>, position: usize) -> Self {
        Self::Parse {
            message: message.into(),
            position,
        }
    }

    pub fn eval(message: impl Into<String>) -> Self {
        Self::Eval(message.into())
    }
}
