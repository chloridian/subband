use core::error::Error;
use core::fmt;
use core::result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    Unsupported,
    TooLarge,
    Malformed,
    Cancelled,
    BufferSize,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => write!(f, "unsupported codestream"),
            Self::TooLarge => write!(f, "image is too large"),
            Self::Malformed => write!(f, "malformed codestream"),
            Self::Cancelled => write!(f, "decoding was cancelled"),
            Self::BufferSize => write!(f, "output buffer has the wrong size"),
        }
    }
}

impl Error for DecodeError {}

pub type Result<T> = result::Result<T, DecodeError>;

macro_rules! bail {
    ($err:expr) => {
        return Err($err.into())
    };
}

pub(crate) use bail;
