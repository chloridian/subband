//! Error types for JPEG 2000 decoding.

use core::fmt;

/// The main error type for JPEG 2000 decoding operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// Errors related to JP2 file format and box parsing.
    Format(FormatError),
    /// Errors related to codestream markers.
    Marker(MarkerError),
    /// Errors related to tile processing.
    Tile(TileError),
    /// Errors related to image dimensions and validation.
    Validation(ValidationError),
    /// Errors related to decoding operations.
    Decoding(DecodingError),
    /// Multi-component transform failed.
    Mct,
    /// Decoding was cancelled.
    Cancelled,
}

/// The error returned when decoding was cancelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

/// Errors related to JP2 file format and box parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatError {
    /// Invalid JP2 signature.
    InvalidSignature,
    /// Invalid JP2 file type.
    InvalidFileType,
    /// Invalid or malformed JP2 box.
    InvalidBox,
    /// Missing codestream data.
    MissingCodestream,
}

/// Errors related to codestream markers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerError {
    /// Invalid marker encountered.
    Invalid,
    /// Unsupported marker encountered.
    Unsupported,
    /// Expected a specific marker.
    Expected(&'static str),
    /// Missing a required marker.
    Missing(&'static str),
    /// Failed to read or parse a marker.
    ParseFailure(&'static str),
}

/// Errors related to tile processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileError {
    /// Invalid image tile was encountered.
    Invalid,
    /// Invalid tile index in tile-part header.
    InvalidIndex,
    /// PPT marker present when PPM marker exists in main header.
    PpmPptConflict,
}

/// Errors related to image dimensions and validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationError {
    /// Invalid image dimensions.
    InvalidDimensions,
    /// Image dimensions exceed supported limits.
    ImageTooLarge,
    /// Invalid component metadata.
    InvalidComponentMetadata,
    /// Invalid progression order.
    InvalidProgressionOrder,
    /// Invalid transformation type.
    InvalidTransformation,
    /// Invalid quantization style.
    InvalidQuantizationStyle,
    /// Missing exponents for precinct sizes.
    MissingPrecinctExponents,
    /// Not enough exponents provided in header.
    InsufficientExponents,
    /// Missing exponent step size.
    MissingStepSize,
    /// Invalid quantization exponents.
    InvalidExponents,
    /// Valid codestream outside what Subband decodes (1-4 unsigned 8-bit components, no subsampling, zero offsets).
    Unsupported,
}

/// Errors related to decoding operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodingError {
    /// An error occurred while decoding a code-block.
    CodeBlockDecodeFailure,
    /// Number of bitplanes in a code-block is too large.
    TooManyBitplanes,
    /// A code-block contains too many coding passes.
    TooManyCodingPasses,
    /// Invalid number of bitplanes in a code-block.
    InvalidBitplaneCount,
    /// A precinct was invalid.
    InvalidPrecinct,
    /// A progression iterator was invalid.
    InvalidProgressionIterator,
    /// Unexpected end of data.
    UnexpectedEof,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Format(e) => write!(f, "{e}"),
            Self::Marker(e) => write!(f, "{e}"),
            Self::Tile(e) => write!(f, "{e}"),
            Self::Validation(e) => write!(f, "{e}"),
            Self::Decoding(e) => write!(f, "{e}"),
            Self::Mct => write!(f, "multi-component transform failed"),
            Self::Cancelled => write!(f, "{Cancelled}"),
        }
    }
}

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "decoding was cancelled")
    }
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSignature => write!(f, "invalid JP2 signature"),
            Self::InvalidFileType => write!(f, "invalid JP2 file type"),
            Self::InvalidBox => write!(f, "invalid JP2 box"),
            Self::MissingCodestream => write!(f, "missing codestream data"),
        }
    }
}

impl fmt::Display for MarkerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid => write!(f, "invalid marker"),
            Self::Unsupported => write!(f, "unsupported marker"),
            Self::Expected(marker) => write!(f, "expected {marker} marker"),
            Self::Missing(marker) => write!(f, "missing {marker} marker"),
            Self::ParseFailure(marker) => write!(f, "failed to parse {marker} marker"),
        }
    }
}

impl fmt::Display for TileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid => write!(f, "image contains no tiles"),
            Self::InvalidIndex => write!(f, "invalid tile index in tile-part header"),
            Self::PpmPptConflict => {
                write!(
                    f,
                    "PPT marker present when PPM marker exists in main header"
                )
            }
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDimensions => write!(f, "invalid image dimensions"),
            Self::ImageTooLarge => write!(f, "image is too large"),
            Self::InvalidComponentMetadata => write!(f, "invalid component metadata"),
            Self::InvalidProgressionOrder => write!(f, "invalid progression order"),
            Self::InvalidTransformation => write!(f, "invalid transformation type"),
            Self::InvalidQuantizationStyle => write!(f, "invalid quantization style"),
            Self::MissingPrecinctExponents => {
                write!(f, "missing exponents for precinct sizes")
            }
            Self::InsufficientExponents => {
                write!(f, "not enough exponents provided in header")
            }
            Self::MissingStepSize => write!(f, "missing exponent step size"),
            Self::InvalidExponents => write!(f, "invalid quantization exponents"),
            Self::Unsupported => write!(f, "unsupported codestream"),
        }
    }
}

impl fmt::Display for DecodingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CodeBlockDecodeFailure => write!(f, "failed to decode code-block"),
            Self::TooManyBitplanes => write!(f, "number of bitplanes is too large"),
            Self::TooManyCodingPasses => {
                write!(f, "code-block contains too many coding passes")
            }
            Self::InvalidBitplaneCount => write!(f, "invalid number of bitplanes"),
            Self::InvalidPrecinct => write!(f, "a precinct was invalid"),
            Self::InvalidProgressionIterator => {
                write!(f, "a progression iterator was invalid")
            }
            Self::UnexpectedEof => write!(f, "unexpected end of data"),
        }
    }
}

impl core::error::Error for DecodeError {}
impl core::error::Error for Cancelled {}
impl core::error::Error for FormatError {}
impl core::error::Error for MarkerError {}
impl core::error::Error for TileError {}
impl core::error::Error for ValidationError {}
impl core::error::Error for DecodingError {}

impl From<Cancelled> for DecodeError {
    fn from(_: Cancelled) -> Self {
        Self::Cancelled
    }
}

impl From<FormatError> for DecodeError {
    fn from(e: FormatError) -> Self {
        Self::Format(e)
    }
}

impl From<MarkerError> for DecodeError {
    fn from(e: MarkerError) -> Self {
        Self::Marker(e)
    }
}

impl From<TileError> for DecodeError {
    fn from(e: TileError) -> Self {
        Self::Tile(e)
    }
}

impl From<ValidationError> for DecodeError {
    fn from(e: ValidationError) -> Self {
        Self::Validation(e)
    }
}

impl From<DecodingError> for DecodeError {
    fn from(e: DecodingError) -> Self {
        Self::Decoding(e)
    }
}

/// Result type for JPEG 2000 decoding operations.
pub type Result<T> = core::result::Result<T, DecodeError>;

macro_rules! bail {
    ($err:expr) => {
        return Err($err.into())
    };
}

macro_rules! err {
    ($err:expr) => {
        Err($err.into())
    };
}

pub(crate) use bail;
pub(crate) use err;
