use core::ops::Range;
use std::vec::Vec;

use super::bitplane::{BitPlaneDecodeBuffers, BitPlaneDecodeContext};
use super::build::{CodeBlock, Decomposition, Layer, Precinct, Segment, SubBand, SubBandType};
use super::codestream::{
    ComponentInfo, Header, MAX_RESOLUTIONS, QuantizationStyle, WaveletTransform,
};
use super::progression::progression_iterator;
use super::rect::IntRect;
use super::tag_tree::TagNode;
use super::tile::{ComponentTile, ResolutionTile, Tile};
use super::{bitplane, build, idwt, mct, segment, tile};
use crate::error::{DecodeError, Result, bail};
use crate::j2c::segment::MAX_BITPLANE_COUNT;
use crate::math;
use crate::reader::BitReader;
use crate::{Decoded, check_cancelled};

#[derive(Clone, Copy)]
pub(crate) struct DecodeParams {
    pub(crate) skipped_resolution_levels: u8,
    pub(crate) max_layers: Option<u8>,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

pub(crate) struct DecodeCtx<'a> {
    pub(crate) codestream: &'a [u8],
    pub(crate) storage: &'a DecompositionStorage,
    pub(crate) header: &'a Header,
    pub(crate) component_info: &'a ComponentInfo,
    pub(crate) max_layers: Option<u8>,
    pub(crate) bit_plane_ctx: &'a mut BitPlaneDecodeContext,
    pub(crate) bit_plane_buffers: &'a mut BitPlaneDecodeBuffers,
    pub(crate) low_row: &'a mut Vec<f32>,
    pub(crate) high_row: &'a mut Vec<f32>,
    pub(crate) cancelled: &'a dyn Fn() -> bool,
}

#[derive(Clone)]
pub(crate) struct TileDecompositions {
    pub(crate) first_ll_sub_band: usize,
    pub(crate) decompositions: Range<usize>,
}

impl TileDecompositions {
    pub(crate) fn sub_band_iter(
        &self,
        resolution: u8,
        decompositions: &[Decomposition],
    ) -> SubBandIter {
        let indices = if resolution == 0 {
            [
                self.first_ll_sub_band,
                self.first_ll_sub_band,
                self.first_ll_sub_band,
            ]
        } else {
            decompositions
                .get(self.decompositions.clone())
                .and_then(|d| d.get(resolution as usize - 1))
                .map_or([0; 3], |d| d.sub_bands)
        };

        SubBandIter {
            next_idx: 0,
            indices,
            resolution,
        }
    }
}

#[derive(Clone)]
pub(crate) struct SubBandIter {
    resolution: u8,
    next_idx: usize,
    indices: [usize; 3],
}

impl Iterator for SubBandIter {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        let value = if self.resolution == 0 {
            if self.next_idx > 0 {
                None
            } else {
                Some(self.indices[0])
            }
        } else {
            self.indices.get(self.next_idx).copied()
        };

        self.next_idx += 1;

        value
    }
}

#[derive(Default)]
pub(crate) struct DecompositionStorage {
    pub(crate) segments: Vec<Segment>,
    pub(crate) layers: Vec<Layer>,
    pub(crate) code_blocks: Vec<CodeBlock>,
    pub(crate) precincts: Vec<Precinct>,
    pub(crate) tag_tree_nodes: Vec<TagNode>,
    pub(crate) sub_bands: Vec<SubBand>,
    pub(crate) decompositions: Vec<Decomposition>,
    pub(crate) tile_decompositions: Vec<TileDecompositions>,
}

impl DecompositionStorage {
    fn reset(&mut self) {
        self.segments.clear();
        self.layers.clear();
        self.code_blocks.clear();
        self.precincts.clear();
        self.sub_bands.clear();
        self.decompositions.clear();
        self.tile_decompositions.clear();
        self.tag_tree_nodes.clear();
    }
}

pub(crate) fn decode(
    data: &[u8],
    header: &Header,
    params: DecodeParams,
    out: &mut [u8],
    cancelled: &dyn Fn() -> bool,
) -> Result<Decoded> {
    check_cancelled(cancelled)?;

    let mut reader = BitReader::new(data);
    let mut tiles = Vec::new();
    let tile_count = tile::parse(&mut reader, header, &mut tiles)?;
    if tile_count == 0 {
        bail!(DecodeError::Malformed);
    }

    let first_tile = tiles.first().ok_or(DecodeError::Malformed)?;
    let num_components = first_tile.component_infos(header).len();
    let decoded_resolutions = usize::from(header.levels - params.skipped_resolution_levels) + 1;
    let mut layers_received = [u8::MAX; MAX_RESOLUTIONS];
    let layers_slice = layers_received
        .get_mut(..decoded_resolutions)
        .ok_or(DecodeError::Malformed)?;

    let mut storage = DecompositionStorage::default();
    let mut bit_plane_ctx = BitPlaneDecodeContext::default();
    let mut bit_plane_buffers = BitPlaneDecodeBuffers::default();
    let mut low_row = Vec::new();
    let mut high_row = Vec::new();
    let mut received_packets = Vec::new();
    let mut expected_packets = Vec::new();

    let tiles = tiles.get(..tile_count).ok_or(DecodeError::Malformed)?;
    for tile in tiles {
        check_cancelled(cancelled)?;

        let component_infos = tile.component_infos(header);
        let tile_mct = mct::tile_mct(tile.mct, component_infos, header.strict)?;
        let first_ci = component_infos.first().ok_or(DecodeError::Malformed)?;
        let res_idx = first_ci
            .num_resolution_levels()
            .checked_sub(1 + params.skipped_resolution_levels)
            .ok_or(DecodeError::Malformed)?;
        let tile_rect = ResolutionTile::new(ComponentTile::new(tile, first_ci), res_idx).rect;

        if tile.tile_parts.is_empty() {
            layers_slice.fill(0);
            fill_tile_rect(
                out,
                params.width as usize,
                num_components,
                tile_rect,
                128,
                cancelled,
            )?;
            continue;
        }

        let num_layers = usize::from(tile.num_layers);
        received_packets.clear();
        received_packets.resize(decoded_resolutions * num_layers, 0);

        let progression_iterator = progression_iterator(tile, header)?;
        storage.reset();

        build::build(
            tile,
            header,
            &mut storage,
            params.skipped_resolution_levels,
            cancelled,
        )?;
        segment::parse(
            data,
            tile,
            progression_iterator,
            header,
            params.skipped_resolution_levels,
            &mut storage,
            &mut received_packets,
            decoded_resolutions,
            cancelled,
        )?;

        let mut ctx = DecodeCtx {
            codestream: data,
            storage: &storage,
            header,
            component_info: first_ci,
            max_layers: params.max_layers,
            bit_plane_ctx: &mut bit_plane_ctx,
            bit_plane_buffers: &mut bit_plane_buffers,
            low_row: &mut low_row,
            high_row: &mut high_row,
            cancelled,
        };
        idwt::apply_tile(&mut ctx, tile_rect, component_infos, tile_mct, params, out)?;

        update_layers_received(
            tile,
            header,
            params.max_layers,
            decoded_resolutions,
            &received_packets,
            &mut expected_packets,
            layers_slice,
        )?;
    }

    Ok(Decoded::new(
        params.width,
        params.height,
        num_components as u8,
        layers_received,
        decoded_resolutions as u8,
    ))
}

fn fill_tile_rect(
    out: &mut [u8],
    image_width: usize,
    num_components: usize,
    rect: IntRect,
    value: u8,
    cancelled: &dyn Fn() -> bool,
) -> Result<()> {
    let width = rect.width() as usize;
    let height = rect.height() as usize;
    if width == 0 || height == 0 {
        return Ok(());
    }
    let x0 = rect.x0 as usize;
    let y0 = rect.y0 as usize;
    let row_bytes = width
        .checked_mul(num_components)
        .ok_or(DecodeError::Malformed)?;
    for y in 0..height {
        check_cancelled(cancelled)?;
        let start = (y0 + y)
            .checked_mul(image_width)
            .and_then(|r| r.checked_add(x0))
            .and_then(|p| p.checked_mul(num_components))
            .ok_or(DecodeError::Malformed)?;
        out.get_mut(start..)
            .and_then(|s| s.get_mut(..row_bytes))
            .ok_or(DecodeError::Malformed)?
            .fill(value);
    }
    Ok(())
}

fn update_layers_received(
    tile: &Tile,
    header: &Header,
    max_layers: Option<u8>,
    decoded_resolutions: usize,
    received_packets: &[u64],
    expected_packets: &mut Vec<u64>,
    layers_received: &mut [u8],
) -> Result<()> {
    let num_layers = usize::from(tile.num_layers);
    let effective_layers = max_layers.map_or(tile.num_layers, |m| m.min(tile.num_layers));

    expected_packets.clear();
    expected_packets.resize(decoded_resolutions * num_layers, 0);
    for data in progression_iterator(tile, header)? {
        let r = usize::from(data.resolution);
        let l = usize::from(data.layer_num);
        if r < decoded_resolutions
            && let Some(slot) = expected_packets.get_mut(r * num_layers + l)
        {
            *slot = slot.saturating_add(1);
        }
    }

    for (r, out) in layers_received.iter_mut().enumerate() {
        let complete = (0..usize::from(effective_layers))
            .take_while(|l| {
                let i = r * num_layers + l;
                let expected = expected_packets.get(i).copied().unwrap_or(0);
                let received = received_packets.get(i).copied().unwrap_or(0);
                expected > 0 && received >= expected
            })
            .count();
        *out = (*out).min(u8::try_from(complete).unwrap_or(u8::MAX));
    }

    Ok(())
}

pub(crate) fn decode_sub_band_strip(
    ctx: &mut DecodeCtx<'_>,
    sub_band_idx: usize,
    resolution: u8,
    y_range: Range<u32>,
    dst: &mut Vec<f32>,
) -> Result<()> {
    let storage = ctx.storage;
    let component_info = ctx.component_info;
    let abs_y0 = y_range.start;
    let abs_y1 = y_range.end;

    let sub_band = storage
        .sub_bands
        .get(sub_band_idx)
        .ok_or(DecodeError::Malformed)?;

    let sb_width = sub_band.rect.width() as usize;
    let strip_height = abs_y1.saturating_sub(abs_y0) as usize;
    if sb_width == 0 || strip_height == 0 {
        return Ok(());
    }

    let strip_len = sb_width
        .checked_mul(strip_height)
        .ok_or(DecodeError::TooLarge)?;
    if dst.len() < strip_len {
        check_cancelled(ctx.cancelled)?;
        dst.resize(strip_len, 0.0);
    }

    let quantised =
        component_info.quantization_info.quantization_style != QuantizationStyle::NoQuantization;
    let irreversible = component_info.wavelet_transform() == WaveletTransform::Irreversible97;
    let dequantization_step = {
        if !quantised {
            1.0
        } else {
            let (exponent, mantissa) =
                component_info.exponent_mantissa(sub_band.sub_band_type, resolution)?;

            let r_b = {
                let log_gain = match sub_band.sub_band_type {
                    SubBandType::LowLow => 0,
                    SubBandType::LowHigh => 1,
                    SubBandType::HighLow => 1,
                    SubBandType::HighHigh => 2,
                };

                8 + log_gain
            };

            math::pow2i(r_b - exponent as i32) * (1.0 + (mantissa as f32) / 2048.0)
        }
    };

    let num_bitplanes = {
        let (exponent, _) = component_info.exponent_mantissa(sub_band.sub_band_type, resolution)?;
        let num_bitplanes = (component_info.quantization_info.guard_bits as u16)
            .checked_add(exponent)
            .and_then(|x| x.checked_sub(1))
            .and_then(|x| x.checked_add(component_info.roi_shift as u16))
            .ok_or(DecodeError::Malformed)?;

        if num_bitplanes > MAX_BITPLANE_COUNT as u16 {
            bail!(DecodeError::Malformed);
        }

        num_bitplanes as u8
    };

    for precinct_idx in sub_band.precincts.clone() {
        let precinct = storage
            .precincts
            .get(precinct_idx)
            .ok_or(DecodeError::Malformed)?;

        if precinct.code_blocks.is_empty() {
            continue;
        }
        let first_cb = storage
            .code_blocks
            .get(precinct.code_blocks.start)
            .ok_or(DecodeError::Malformed)?;
        let last_cb = storage
            .code_blocks
            .get(precinct.code_blocks.end - 1)
            .ok_or(DecodeError::Malformed)?;
        if first_cb.rect.y0 >= abs_y1 || last_cb.rect.y1 <= abs_y0 {
            continue;
        }

        for code_block_idx in precinct.code_blocks.clone() {
            let code_block = storage
                .code_blocks
                .get(code_block_idx)
                .ok_or(DecodeError::Malformed)?;

            if code_block.rect.y0 >= abs_y1 || code_block.rect.y1 <= abs_y0 {
                continue;
            }

            check_cancelled(ctx.cancelled)?;

            let x_offset = code_block.rect.x0 - sub_band.rect.x0;
            let y_offset = code_block.rect.y0 - abs_y0;
            let mut base_idx = (y_offset as usize) * sb_width + x_offset as usize;

            if !bitplane::decode(
                ctx.codestream,
                code_block,
                sub_band.sub_band_type,
                num_bitplanes,
                &component_info.coding_style.parameters.code_block_style,
                ctx.bit_plane_ctx,
                ctx.bit_plane_buffers,
                storage,
                ctx.max_layers,
                ctx.header.strict,
            )? {
                let cb_w = code_block.rect.width() as usize;
                let cb_h = code_block.rect.height() as usize;
                for _ in 0..cb_h {
                    let out_row = dst
                        .get_mut(base_idx..)
                        .and_then(|r| r.get_mut(..cb_w))
                        .ok_or(DecodeError::Malformed)?;
                    out_row.fill(0.0);
                    base_idx += sb_width;
                }
                continue;
            }

            ctx.bit_plane_ctx.write_reconstructed(
                dst,
                base_idx,
                sb_width,
                irreversible,
                component_info.roi_shift,
                dequantization_step,
            )?;
        }
    }

    Ok(())
}
