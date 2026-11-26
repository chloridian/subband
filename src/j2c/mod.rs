mod arithmetic_decoder;
mod bitplane;
mod build;
mod codestream;
mod decode;
mod idwt;
mod mct;
mod progression;
mod rect;
mod segment;
mod tag_tree;
mod tile;

use crate::error::{DecodeError, Result, bail};
use crate::j2c::codestream::markers;
use crate::reader::BitReader;
use crate::{DecodeSettings, Image};

use crate::math::{SIMD_WIDTH, SimdBuffer};
pub(crate) use codestream::Header;
pub use decode::DecoderContext;
pub(crate) use decode::decode;

pub(crate) struct ParsedCodestream<'a> {
    pub(crate) header: Header,
    pub(crate) data: &'a [u8],
}

/// Decoded data for one JPEG2000 component.
#[derive(Debug, Clone)]
pub struct ComponentData {
    pub(crate) container: SimdBuffer<{ SIMD_WIDTH }>,
}

impl ComponentData {
    /// The decoded samples of this component.
    pub fn samples(&self) -> &[f32] {
        self.container.truncated()
    }
}

pub(crate) fn parse<'a>(stream: &'a [u8], settings: &DecodeSettings) -> Result<Image<'a>> {
    let parsed_codestream = parse_raw(stream, settings)?;

    Ok(Image {
        codestream: parsed_codestream.data,
        header: parsed_codestream.header,
        settings: settings.clone(),
    })
}

pub(crate) fn parse_raw<'a>(
    stream: &'a [u8],
    settings: &DecodeSettings,
) -> Result<ParsedCodestream<'a>> {
    let mut reader = BitReader::new(stream);

    let marker = reader.read_marker()?;
    if marker != markers::SOC {
        bail!(DecodeError::Malformed);
    }

    let header = codestream::read_header(&mut reader, settings)?;
    let code_stream_data = reader.tail().ok_or(DecodeError::Malformed)?;

    Ok(ParsedCodestream {
        header,
        data: code_stream_data,
    })
}
