/*!
Subband is a fork of [hayro-jpeg2000](https://github.com/LaurenzV/hayro), tuned for the
textures Second Life serves: raw JPEG 2000 codestreams (`.j2c`) from untrusted sources,
decoded at reduced resolution and cancelled when no longer needed. It also unwraps the
JP2 container format to decode its codestream. An encoder is planned.

# Example
```rust,no_run
use subband::{DecodeSettings, DecoderContext, Image};

let data = std::fs::read("texture.j2c").unwrap();
let settings = DecodeSettings {
    strict: true,
    target_resolution: Some((512, 512)),
    ..DecodeSettings::default()
};
let image = Image::from_codestream(&data, 3, &settings).unwrap();

let mut ctx = DecoderContext::default();
let decoded = image.decode(&mut ctx).unwrap();
let pixels = decoded.data_u8().unwrap();
```

# Testing
Before the fork, hayro-jpeg2000 was tested against more than 20,000 images taken from
PDFs, and it passes a large part of the `OpenJPEG` test suite.

# Safety
By default, the crate has the `simd` feature enabled, which uses the
[`fearless_simd`](https://github.com/linebender/fearless_simd) crate to accelerate
important parts of the pipeline. If you want to eliminate any usage of unsafe
in this crate as well as its dependencies, you can simply disable this
feature, at the cost of slower decoding. Unsafe code is forbidden
via a crate-level attribute.

The crate is `no_std` compatible but requires an allocator to be available.
*/

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![forbid(missing_docs)]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

use crate::error::err;
use crate::j2c::Header;

mod cancel;
pub mod error;
pub(crate) mod math;
mod sync;

use cancel::{CHUNK, Cancel};
pub use cancel::{CancelCheck, CancelHandle};

use crate::math::{Level, Simd, dispatch};
pub use error::{
    Cancelled, ColorError, DecodeError, DecodingError, FormatError, MarkerError, Result, TileError,
    ValidationError,
};
pub use j2c::{ComponentData, DecoderContext};
pub use jp2::DecodedImage;

mod j2c;
mod jp2;
pub(crate) mod reader;

pub(crate) const JP2_MAGIC: &[u8] = b"\x00\x00\x00\x0C\x6A\x50\x20\x20";
pub(crate) const CODESTREAM_MAGIC: &[u8] = b"\xFF\x4F\xFF\x51";

/// Settings to apply during decoding.
#[derive(Debug, Clone, Default)]
pub struct DecodeSettings {
    /// Whether to reject malformed input instead of decoding it leniently.
    pub strict: bool,
    /// A hint for the target resolution that the image should be decoded at.
    pub target_resolution: Option<(u32, u32)>,
    /// A check for cancelling decoding.
    pub cancel: Option<CancelHandle>,
}

/// A JPEG2000 image or codestream.
pub struct Image<'a> {
    pub(crate) codestream: &'a [u8],
    pub(crate) header: Header,
    pub(crate) settings: DecodeSettings,
    pub(crate) has_alpha: bool,
    pub(crate) color_space: ColorSpace,
}

impl<'a> Image<'a> {
    /// Try to create a new JPEG2000 image from the given data.
    pub fn new(data: &'a [u8], settings: &DecodeSettings) -> Result<Self> {
        if data.starts_with(JP2_MAGIC) {
            jp2::parse(data, settings.clone())
        } else if data.starts_with(CODESTREAM_MAGIC) {
            j2c::parse(data, settings, None)
        } else {
            err!(FormatError::InvalidSignature)
        }
    }

    /// Try to create a new image from a raw codestream with the given number of colour channels.
    pub fn from_codestream(
        data: &'a [u8],
        color_channels: u8,
        settings: &DecodeSettings,
    ) -> Result<Self> {
        if data.starts_with(CODESTREAM_MAGIC) {
            j2c::parse(data, settings, Some(color_channels))
        } else {
            err!(FormatError::InvalidSignature)
        }
    }

    /// Whether the image has an alpha channel.
    pub fn has_alpha(&self) -> bool {
        self.has_alpha
    }

    /// The color space of the image.
    pub fn color_space(&self) -> &ColorSpace {
        &self.color_space
    }

    /// The width of the image.
    pub fn width(&self) -> u32 {
        self.header.size_data.image_width()
    }

    /// The height of the image.
    pub fn height(&self) -> u32 {
        self.header.size_data.image_height()
    }

    /// The original bit depth of the image.
    pub fn original_bit_depth(&self) -> u8 {
        // Assumes all components share the same precision.
        self.header.component_infos[0].size_info.precision
    }

    /// Decode the image and return its decoded components.
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

pub(crate) fn resolve_alpha_and_color_space(header: &Header) -> Result<(ColorSpace, bool)> {
    match header.component_infos.len() {
        1 => Ok((ColorSpace::Gray, false)),
        2 => Ok((ColorSpace::Gray, true)),
        3 => Ok((ColorSpace::RGB, false)),
        4 => Ok((ColorSpace::RGB, true)),
        _ => err!(FormatError::Unsupported),
    }
}

/// The color space of the image.
#[derive(Debug, Clone)]
pub enum ColorSpace {
    /// A grayscale image.
    Gray,
    /// An RGB image.
    RGB,
}

impl ColorSpace {
    /// Return the number of expected channels for the color space.
    pub fn num_channels(&self) -> u8 {
        match self {
            Self::Gray => 1,
            Self::RGB => 3,
        }
    }
}

/// A bitmap storing the decoded result of the image.
pub struct Bitmap {
    /// The color space of the image.
    pub color_space: ColorSpace,
    /// The interleaved 8-bit samples, with the alpha channel last.
    pub data: Vec<u8>,
    /// Whether the image has an alpha channel.
    pub has_alpha: bool,
    /// The width of the image.
    pub width: u32,
    /// The height of the image.
    pub height: u32,
    /// The original bit depth of the image.
    pub original_bit_depth: u8,
}

impl DecodedImage<'_> {
    /// The decoded components of the image.
    pub fn components(&self) -> &[ComponentData] {
        self.decoded_components
    }

    /// Return the decoded image as interleaved unsigned 8-bit sample data.
    pub fn data_u8(&self) -> core::result::Result<Vec<u8>, Cancelled> {
        let components = self.components();
        let buffer_size = components[0].samples().len() * components.len();
        let mut buf = vec![0; buffer_size];
        self.store_u8_into(&mut buf)?;

        Ok(buf)
    }

    /// Store the decoded image as interleaved 8-bit samples into `buf`, which must fit exactly.
    pub fn store_u8_into(&self, buf: &mut [u8]) -> core::result::Result<(), Cancelled> {
        interleave_and_convert(self, buf)
    }
}

fn interleave_and_convert(
    image: &DecodedImage<'_>,
    buf: &mut [u8],
) -> core::result::Result<(), Cancelled> {
    let cancel = image.cancel;
    let components = &*image.decoded_components;
    let num_components = components.len();

    let mut all_same_bit_depth = Some(components[0].bit_depth);

    for component in components.iter().skip(1) {
        if Some(component.bit_depth) != all_same_bit_depth {
            all_same_bit_depth = None;
        }
    }

    let max_len = components[0].container.truncated().len();

    let mut output_iter = buf.iter_mut();

    fn to_u8_scale(bit_depth: u8) -> f32 {
        255.0 / ((1_u32 << bit_depth) - 1) as f32
    }

    if let Some(bit_depth) = all_same_bit_depth
        && num_components <= 4
    {
        let scale = to_u8_scale(bit_depth);
        let max_len = max_len.min(buf.len() / num_components);
        let mut start = 0;
        while start < max_len {
            cancel.check()?;

            let end = (start + CHUNK).min(max_len);
            let plane = |c: usize| &components[c].container[start..end];
            let out = &mut buf[start * num_components..end * num_components];
            match num_components {
                1 => interleave_to_u8([plane(0)], scale, out),
                2 => interleave_to_u8([plane(0), plane(1)], scale, out),
                3 => interleave_to_u8([plane(0), plane(1), plane(2)], scale, out),
                4 => interleave_to_u8([plane(0), plane(1), plane(2), plane(3)], scale, out),
                _ => unreachable!(),
            }
            start = end;
        }
    } else {
        let scales = components
            .iter()
            .map(|c| to_u8_scale(c.bit_depth))
            .collect::<Vec<_>>();

        for sample in 0..max_len {
            if sample % CHUNK == 0 {
                cancel.check()?;
            }

            for (channel, scale) in components.iter().zip(&scales) {
                *output_iter.next().unwrap() = math::round_to_u8(channel.container[sample] * scale);
            }
        }
    }

    Ok(())
}

fn interleave_to_u8<const N: usize>(planes: [&[f32]; N], scale: f32, out: &mut [u8]) {
    dispatch!(Level::new(), simd => interleave_to_u8_impl(simd, planes, scale, out));
}

#[inline(always)]
fn interleave_to_u8_impl<S: Simd, const N: usize>(
    simd: S,
    planes: [&[f32]; N],
    scale: f32,
    out: &mut [u8],
) {
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
                math::round_and_interleave(simd, [c0], scale, out);
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
                math::round_and_interleave(simd, [c0, c1], scale, out);
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
                math::round_and_interleave(simd, [c0, c1, c2], scale, out);
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
                math::round_and_interleave(simd, [c0, c1, c2, c3], scale, out);
            }
        }
        _ => unreachable!(),
    }

    for (i, pixel) in tail.as_chunks_mut::<N>().0.iter_mut().enumerate() {
        for (value, plane) in pixel.iter_mut().zip(planes) {
            *value = math::round_to_u8(plane[blocks + i] * scale);
        }
    }
}
