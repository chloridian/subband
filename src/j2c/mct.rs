use core::array;

use super::codestream::{ComponentInfo, WaveletTransform};
use crate::error::{DecodeError, Result, bail};
use crate::math::{self, SIMD_WIDTH, Simd, f32x8};

pub(crate) fn tile_mct(
    enabled: bool,
    component_infos: &[ComponentInfo],
    strict: bool,
) -> Result<Option<WaveletTransform>> {
    if !enabled {
        return Ok(None);
    }

    let [c0, c1, c2, ..] = component_infos else {
        return if strict {
            Err(DecodeError::Malformed)
        } else {
            Ok(None)
        };
    };

    let transform = c0.wavelet_transform();
    if transform != c1.wavelet_transform() || transform != c2.wavelet_transform() {
        bail!(DecodeError::Malformed);
    }

    Ok(Some(transform))
}

#[inline(always)]
pub(crate) fn write_row<S: Simd>(
    simd: S,
    rows: &[&[f32]],
    mct: Option<WaveletTransform>,
    out: &mut [u8],
) {
    match rows {
        [r0] => write_row_n(simd, [*r0], None, out),
        [r0, r1] => write_row_n(simd, [*r0, *r1], None, out),
        [r0, r1, r2] => write_row_n(simd, [*r0, *r1, *r2], mct, out),
        [r0, r1, r2, r3] => write_row_n(simd, [*r0, *r1, *r2, *r3], mct, out),
        _ => {}
    }
}

#[inline(always)]
fn transform_8<S: Simd, const N: usize>(
    simd: S,
    mut v: [f32x8<S>; N],
    mct: Option<WaveletTransform>,
) -> [f32x8<S>; N] {
    if let (Some(transform), [c0, c1, c2, ..]) = (mct, v.as_mut_slice()) {
        let y0 = *c0;
        let y1 = *c1;
        let y2 = *c2;
        match transform {
            WaveletTransform::Irreversible97 => {
                *c0 = y2.mul_add(f32x8::splat(simd, 1.402), y0);
                *c1 = y2.mul_add(
                    f32x8::splat(simd, -0.71414),
                    y1.mul_add(f32x8::splat(simd, -0.34413), y0),
                );
                *c2 = y1.mul_add(f32x8::splat(simd, 1.772), y0);
            }
            WaveletTransform::Reversible53 => {
                let i1 = y0 - ((y2 + y1) * 0.25).floor();
                *c0 = y2 + i1;
                *c1 = i1;
                *c2 = y1 + i1;
            }
        }
    }

    let offset = f32x8::splat(simd, 128.0);
    for c in &mut v {
        *c += offset;
    }
    v
}

#[inline(always)]
fn write_row_n<S: Simd, const N: usize>(
    simd: S,
    rows: [&[f32]; N],
    mct: Option<WaveletTransform>,
    out: &mut [u8],
) {
    let pixels = rows
        .iter()
        .map(|row| row.len())
        .fold(out.len() / N, usize::min);
    let blocks = pixels / 16 * 16;
    let Some(out) = out.get_mut(..pixels * N) else {
        return;
    };
    let (out_main, mut out_tail) = out.split_at_mut(blocks * N);

    let mut offset = 0;
    for chunk in out_main.chunks_exact_mut(16 * N) {
        let lo: [_; N] = transform_8(
            simd,
            array::from_fn(|c| {
                let slice = rows
                    .get(c)
                    .and_then(|r| r.get(offset..offset + SIMD_WIDTH))
                    .unwrap_or(&[]);
                f32x8::from_slice(simd, slice)
            }),
            mct,
        );
        let hi: [_; N] = transform_8(
            simd,
            array::from_fn(|c| {
                let slice = rows
                    .get(c)
                    .and_then(|r| r.get(offset + SIMD_WIDTH..offset + 16))
                    .unwrap_or(&[]);
                f32x8::from_slice(simd, slice)
            }),
            mct,
        );
        let zero = f32x8::splat(simd, 0.0);
        let mut pairs_iter = lo.into_iter().zip(hi);
        let pairs: [_; N] = array::from_fn(|_| pairs_iter.next().unwrap_or((zero, zero)));
        math::round_and_interleave_f32x8(simd, pairs, chunk);
        offset += 16;
    }

    while offset < pixels {
        let count = (pixels - offset).min(SIMD_WIDTH);
        let v: [_; N] = transform_8(
            simd,
            array::from_fn(|c| {
                let mut tmp = [0.0_f32; SIMD_WIDTH];
                if let Some(src) = rows.get(c).and_then(|r| r.get(offset..offset + count))
                    && let Some(dst) = tmp.get_mut(..count)
                {
                    dst.copy_from_slice(src);
                }
                f32x8::from_slice(simd, &tmp)
            }),
            mct,
        );
        let mut stored = [[0.0_f32; SIMD_WIDTH]; N];
        for (dst, vec) in stored.iter_mut().zip(v) {
            vec.store(dst);
        }
        let (chunk, rest) = out_tail.split_at_mut(count * N);
        out_tail = rest;
        for (i, pixel) in chunk.as_chunks_mut::<N>().0.iter_mut().enumerate() {
            for (value, channel) in pixel.iter_mut().zip(&stored) {
                if let Some(&sample) = channel.get(i) {
                    *value = math::round_to_u8(sample);
                }
            }
        }
        offset += count;
    }
}
