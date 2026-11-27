#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

mod cancel;
mod error;
mod j2c;
mod jp2;
pub(crate) mod math;
pub(crate) mod reader;

use crate::cancel::{CHUNK, Cancel};
pub use crate::error::{DecodeError, Result};
pub use crate::j2c::Inspection;
use crate::j2c::{ComponentData, DecodeParams, DecoderContext, Header, MAX_RESOLUTIONS};
use crate::math::{Level, Simd, dispatch};

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
        let cancel = Cancel(&cancelled);
        let mut ctx = DecoderContext::default();
        let decoded = j2c::decode(self.codestream, &self.header, params, &mut ctx, cancel)?;
        let num_components = self.header.component_infos.len();
        let decoded_components = ctx
            .channel_data
            .get(..num_components)
            .ok_or(DecodeError::Malformed)?;
        interleave_and_convert(decoded_components, out, cancel)?;

        Ok(decoded)
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

fn interleave_and_convert(
    components: &[ComponentData],
    buf: &mut [u8],
    cancel: Cancel<'_>,
) -> Result<()> {
    let Some(first) = components.first() else {
        return Ok(());
    };
    let num_components = components.len();
    let max_len = first.container.truncated().len();
    let mut start = 0;

    while start < max_len {
        cancel.check()?;

        let end = (start + CHUNK).min(max_len);
        let Some(out) = buf.get_mut(start * num_components..end * num_components) else {
            break;
        };
        match components {
            [c0] => {
                if let Some(p0) = c0.container.get(start..end) {
                    interleave_to_u8([p0], out);
                }
            }
            [c0, c1] => {
                if let (Some(p0), Some(p1)) =
                    (c0.container.get(start..end), c1.container.get(start..end))
                {
                    interleave_to_u8([p0, p1], out);
                }
            }
            [c0, c1, c2] => {
                if let (Some(p0), Some(p1), Some(p2)) = (
                    c0.container.get(start..end),
                    c1.container.get(start..end),
                    c2.container.get(start..end),
                ) {
                    interleave_to_u8([p0, p1, p2], out);
                }
            }
            [c0, c1, c2, c3] => {
                if let (Some(p0), Some(p1), Some(p2), Some(p3)) = (
                    c0.container.get(start..end),
                    c1.container.get(start..end),
                    c2.container.get(start..end),
                    c3.container.get(start..end),
                ) {
                    interleave_to_u8([p0, p1, p2, p3], out);
                }
            }
            _ => unreachable!(),
        }
        start = end;
    }

    Ok(())
}

fn interleave_to_u8<const N: usize>(planes: [&[f32]; N], out: &mut [u8]) {
    dispatch!(Level::new(), simd => interleave_to_u8_impl(simd, planes, out));
}

#[inline(always)]
fn interleave_to_u8_impl<S: Simd, const N: usize>(simd: S, planes: [&[f32]; N], out: &mut [u8]) {
    let pixels = planes
        .iter()
        .map(|plane| plane.len())
        .fold(out.len() / N, usize::min);
    let blocks = pixels / 16 * 16;
    let Some(out) = out.get_mut(..pixels * N) else {
        return;
    };
    let (out, tail) = out.split_at_mut(blocks * N);
    let plane = |c: usize| {
        planes
            .get(c)
            .and_then(|p| p.get(..blocks))
            .unwrap_or(&[])
            .as_chunks::<16>()
            .0
    };

    match N {
        1 => {
            for (out, c0) in out.as_chunks_mut::<16>().0.iter_mut().zip(plane(0)) {
                math::round_and_interleave(simd, [c0], out);
            }
        }
        2 => {
            for ((out, c0), c1) in out
                .as_chunks_mut::<32>()
                .0
                .iter_mut()
                .zip(plane(0))
                .zip(plane(1))
            {
                math::round_and_interleave(simd, [c0, c1], out);
            }
        }
        3 => {
            for (((out, c0), c1), c2) in out
                .as_chunks_mut::<48>()
                .0
                .iter_mut()
                .zip(plane(0))
                .zip(plane(1))
                .zip(plane(2))
            {
                math::round_and_interleave(simd, [c0, c1, c2], out);
            }
        }
        4 => {
            for ((((out, c0), c1), c2), c3) in out
                .as_chunks_mut::<64>()
                .0
                .iter_mut()
                .zip(plane(0))
                .zip(plane(1))
                .zip(plane(2))
                .zip(plane(3))
            {
                math::round_and_interleave(simd, [c0, c1, c2, c3], out);
            }
        }
        _ => unreachable!(),
    }

    for (i, pixel) in tail.as_chunks_mut::<N>().0.iter_mut().enumerate() {
        for (value, plane) in pixel.iter_mut().zip(planes) {
            if let Some(&sample) = plane.get(blocks + i) {
                *value = math::round_to_u8(sample);
            }
        }
    }
}
