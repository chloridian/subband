#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

mod error;
mod j2c;
mod jp2;
pub(crate) mod math;
pub(crate) mod reader;

pub use crate::error::{DecodeError, Result};
pub use crate::j2c::Inspection;
use crate::j2c::{DecodeParams, Header, MAX_RESOLUTIONS};

pub(crate) const JP2_MAGIC: &[u8] = b"\x00\x00\x00\x0C\x6A\x50\x20\x20";
pub(crate) const CODESTREAM_MAGIC: &[u8] = b"\xFF\x4F\xFF\x51";

const DEFAULT_MAX_EDGE: u32 = 4096;
const DEFAULT_MAX_INPUT: usize = 32 * 1024 * 1024;
const DEFAULT_MAX_OUTPUT: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct DecodeSettings {
    pub strict: bool,
    pub max_edge: u32,
    pub max_input: usize,
    pub max_output: usize,
}

impl Default for DecodeSettings {
    fn default() -> Self {
        Self {
            strict: false,
            max_edge: DEFAULT_MAX_EDGE,
            max_input: DEFAULT_MAX_INPUT,
            max_output: DEFAULT_MAX_OUTPUT,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Decode {
    pub discard: u8,
    pub max_layers: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decoded {
    width: u32,
    height: u32,
    components: u8,
    layers_received: [u8; MAX_RESOLUTIONS],
    resolutions: u8,
}

impl Decoded {
    pub(crate) fn new(
        width: u32,
        height: u32,
        components: u8,
        layers_received: [u8; MAX_RESOLUTIONS],
        resolutions: u8,
    ) -> Self {
        Self {
            width,
            height,
            components,
            layers_received,
            resolutions,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn components(&self) -> u8 {
        self.components
    }

    pub fn layers_received(&self) -> &[u8] {
        self.layers_received
            .get(..usize::from(self.resolutions))
            .unwrap_or(&self.layers_received)
    }
}

pub struct Image<'a> {
    pub(crate) codestream: &'a [u8],
    pub(crate) header: Header,
    pub(crate) settings: DecodeSettings,
}

impl<'a> Image<'a> {
    pub fn new(data: &'a [u8], settings: &DecodeSettings) -> Result<Self> {
        if data.len() > settings.max_input {
            return Err(DecodeError::TooLarge);
        }

        if data.starts_with(JP2_MAGIC) {
            jp2::parse(data, *settings)
        } else if data.starts_with(CODESTREAM_MAGIC) {
            j2c::parse(data, settings)
        } else {
            Err(DecodeError::Malformed)
        }
    }

    pub fn from_codestream(data: &'a [u8], settings: &DecodeSettings) -> Result<Self> {
        if data.len() > settings.max_input {
            return Err(DecodeError::TooLarge);
        }

        if data.starts_with(CODESTREAM_MAGIC) {
            j2c::parse(data, settings)
        } else {
            Err(DecodeError::Malformed)
        }
    }

    pub fn width(&self) -> u32 {
        self.header.size_data.reference_grid_width
    }

    pub fn height(&self) -> u32 {
        self.header.size_data.reference_grid_height
    }

    pub fn components(&self) -> u8 {
        self.header.component_infos.len() as u8
    }

    pub fn levels(&self) -> u8 {
        self.header.levels
    }

    pub fn layers(&self) -> u8 {
        self.header.global_coding_style.num_layers
    }

    pub fn output_size(&self, request: &Decode) -> [u32; 2] {
        let discard = request.discard.min(self.header.levels);
        let factor = 1_u32 << discard;
        [
            self.header.size_data.reference_grid_width.div_ceil(factor),
            self.header.size_data.reference_grid_height.div_ceil(factor),
        ]
    }

    pub fn output_len(&self, request: &Decode) -> usize {
        let [width, height] = self.output_size(request);
        (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(usize::from(self.components()))
    }

    pub fn decode_into(
        &self,
        request: &Decode,
        out: &mut [u8],
        cancelled: impl Fn() -> bool,
    ) -> Result<Decoded> {
        let discard = request.discard.min(self.header.levels);
        let [width, height] = self.output_size(request);
        let output_bytes = u64::from(width)
            .saturating_mul(u64::from(height))
            .saturating_mul(u64::from(self.header.size_data.num_components));
        if output_bytes > self.settings.max_output as u64 {
            return Err(DecodeError::TooLarge);
        }
        let expected_len = usize::try_from(output_bytes).map_err(|_| DecodeError::TooLarge)?;
        if out.len() != expected_len {
            return Err(DecodeError::BufferSize);
        }

        let params = DecodeParams {
            skipped_resolution_levels: discard,
            max_layers: request.max_layers.map(|n| n.max(1)),
            width,
            height,
        };
        j2c::decode(self.codestream, &self.header, params, out, &cancelled)
    }
}

pub(crate) fn check_cancelled(cancelled: &dyn Fn() -> bool) -> Result<()> {
    if cancelled() {
        Err(DecodeError::Cancelled)
    } else {
        Ok(())
    }
}

pub fn inspect(
    data: &[u8],
    settings: &DecodeSettings,
    max_layers: Option<u8>,
) -> Result<Inspection> {
    if data.len() > settings.max_input {
        return Err(DecodeError::TooLarge);
    }

    if data.starts_with(CODESTREAM_MAGIC) {
        j2c::inspect(data, settings, max_layers)
    } else {
        Err(DecodeError::Malformed)
    }
}
