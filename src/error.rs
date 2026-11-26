use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    Unsupported,
    TooLarge,
    Malformed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => write!(f, "unsupported codestream"),
            Self::TooLarge => write!(f, "image is too large"),
            Self::Malformed => write!(f, "malformed codestream"),
            Self::Cancelled => write!(f, "{Cancelled}"),
        }
    }
}

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "decoding was cancelled")
    }
}

impl core::error::Error for DecodeError {}
impl core::error::Error for Cancelled {}

impl From<Cancelled> for DecodeError {
    fn from(_: Cancelled) -> Self {
        Self::Cancelled
    }
}

pub type Result<T> = core::result::Result<T, DecodeError>;

macro_rules! bail {
    ($err:expr) => {
        return Err($err.into())
    };
}

pub(crate) use bail;
