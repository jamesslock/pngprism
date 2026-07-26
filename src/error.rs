//! Typed crate-wide errors with oracle-verbatim display text.

use std::fmt;

/// Stable machine-readable classification for every public failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Input/output failure.
    Io,
    /// Malformed, unsupported, or otherwise invalid caller data.
    Data,
    /// A violated implementation invariant.
    Internal,
    /// Invalid command-line syntax or composition.
    Usage,
}

/// One crate-wide error carrying a stable kind beside verbatim message text.
///
/// The message is deliberately opaque: it remains byte-identical to the
/// Python oracle while callers route on [`kind`](Self::kind), never prefixes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    kind: Kind,
    message: String,
}

impl Error {
    /// Construct an error without interpreting or rewriting its message.
    pub fn new(kind: Kind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Return the stable machine-readable classification.
    pub const fn kind(&self) -> Kind {
        self.kind
    }

    /// Return the oracle-verbatim display message.
    pub fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn io(message: impl Into<String>) -> Self {
        Self::new(Kind::Io, message)
    }

    pub(crate) fn data(message: impl Into<String>) -> Self {
        Self::new(Kind::Data, message)
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self::new(Kind::Internal, message)
    }

    pub(crate) fn usage(message: impl Into<String>) -> Self {
        Self::new(Kind::Usage, message)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<(Kind, String)> for Error {
    fn from((kind, message): (Kind, String)) -> Self {
        Self::new(kind, message)
    }
}

impl From<(Kind, &'static str)> for Error {
    fn from((kind, message): (Kind, &'static str)) -> Self {
        Self::new(kind, message)
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::io(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_preserves_message_bytes_for_every_kind() {
        let messages = [
            (Kind::Io, "io_error: cannot read fixture: denied"),
            (
                Kind::Data,
                "data_error: cannot decode fixture: missing PNG signature",
            ),
            (
                Kind::Internal,
                "internal: emitted pixels differ from remap candidate",
            ),
            (
                Kind::Usage,
                "usage_error: --dither-strength must be a decimal in 0..1",
            ),
        ];

        for (kind, message) in messages {
            let error = Error::new(kind, message);
            assert_eq!(error.to_string().as_bytes(), message.as_bytes());
        }
    }

    #[test]
    fn tuple_conversions_preserve_kind_and_message() {
        let owned = Error::from((Kind::Data, "owned message".to_string()));
        let borrowed = Error::from((Kind::Usage, "borrowed message"));

        assert_eq!(
            (owned.kind(), owned.message()),
            (Kind::Data, "owned message")
        );
        assert_eq!(
            (borrowed.kind(), borrowed.message()),
            (Kind::Usage, "borrowed message")
        );
    }

    #[test]
    fn io_conversion_is_typed_without_rewriting_the_source_message() {
        let error = Error::from(std::io::Error::other("read failed"));

        assert_eq!((error.kind(), error.message()), (Kind::Io, "read failed"));
    }
}
