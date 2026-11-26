#![forbid(unsafe_code)]

use core::result::Result as CoreResult;
use std::vec;
use std::vec::Vec;

use crate::j2c::Header;

mod cancel;
pub mod error;
pub(crate) mod math;

use cancel::{CHUNK, Cancel};
pub use cancel::{CancelCheck, CancelHandle};

use crate::math::{Level, Simd, dispatch};
pub use error::{Cancelled, DecodeError, Result};
pub use j2c::{ComponentData, DecoderContext};
pub use jp2::DecodedImage;

mod j2c;
mod jp2;
pub(crate) mod reader;

pub(crate) const JP2_MAGIC: &[u8] = b"\x00\x00\x00\x0C\x6A\x50\x20\x20";
pub(crate) const CODESTREAM_MAGIC: &[u8] = b"\xFF\x4F\xFF\x51";

const DEFAULT_MAX_EDGE: u32 = 4096;
const DEFAULT_MAX_INPUT: usize = 32 * 1024 * 1024;
const DEFAULT_MAX_OUTPUT: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct DecodeSettings {
    pub strict: bool,
    pub target_resolution: Option<(u32, u32)>,
    pub cancel: Option<CancelHandle>,
    pub max_edge: u32,
    pub max_input: usize,
    pub max_output: usize,
}

impl Default for DecodeSettings {
    fn default() -> Self {
        Self {
            strict: false,
            target_resolution: None,
            cancel: None,
            max_edge: DEFAULT_MAX_EDGE,
            max_input: DEFAULT_MAX_INPUT,
            max_output: DEFAULT_MAX_OUTPUT,
        }
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
            jp2::parse(data, settings.clone())
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
        self.header.size_data.image_width()
    }

    pub fn height(&self) -> u32 {
        self.header.size_data.image_height()
    }

    pub fn decode<'b>(
        &'a self,
        decoder_context: &'b mut DecoderContext<'a>,
    ) -> Result<DecodedImage<'b>> {
        let settings = &self.settings;
        let cancel = Cancel(settings.cancel.as_ref());
        j2c::decode(self.codestream, &self.header, decoder_context, cancel)?;
        Ok(DecodedImage {
            decoded_components: &mut decoder_context.channel_data,
            cancel,
        })
    }
}

impl DecodedImage<'_> {
    pub fn components(&self) -> &[ComponentData] {
        self.decoded_components
    }

    pub fn data_u8(&self) -> CoreResult<Vec<u8>, Cancelled> {
        let components = self.components();
        let buffer_size = components[0].samples().len() * components.len();
        let mut buf = vec![0; buffer_size];
        self.store_u8_into(&mut buf)?;

        Ok(buf)
    }

    pub fn store_u8_into(&self, buf: &mut [u8]) -> CoreResult<(), Cancelled> {
        interleave_and_convert(self, buf)
    }
}

fn interleave_and_convert(image: &DecodedImage<'_>, buf: &mut [u8]) -> CoreResult<(), Cancelled> {
    let cancel = image.cancel;
    let components = &*image.decoded_components;
    let num_components = components.len();
    let max_len = components[0]
        .container
        .truncated()
        .len()
        .min(buf.len() / num_components);
    let mut start = 0;

    while start < max_len {
        cancel.check()?;

        let end = (start + CHUNK).min(max_len);
        let plane = |c: usize| &components[c].container[start..end];
        let out = &mut buf[start * num_components..end * num_components];
        match num_components {
            1 => interleave_to_u8([plane(0)], out),
            2 => interleave_to_u8([plane(0), plane(1)], out),
            3 => interleave_to_u8([plane(0), plane(1), plane(2)], out),
            4 => interleave_to_u8([plane(0), plane(1), plane(2), plane(3)], out),
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
    let (out, tail) = out[..pixels * N].split_at_mut(blocks * N);
    let plane = |c: usize| planes[c][..blocks].as_chunks::<16>().0;

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
            *value = math::round_to_u8(plane[blocks + i]);
        }
    }
}
