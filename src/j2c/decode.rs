use core::ops::Range;
use std::vec::Vec;

use super::bitplane::{BitPlaneDecodeBuffers, BitPlaneDecodeContext};
use super::build::{CodeBlock, Decomposition, Layer, Precinct, Segment, SubBand, SubBandType};
use super::codestream::{ComponentInfo, Header, QuantizationStyle, WaveletTransform};
use super::idwt::{IDWTOutput, IdwtScratch};
use super::progression::{ProgressionIterator, progression_iterator};
use super::tag_tree::TagNode;
use super::tile::{ComponentTile, ResolutionTile, Tile};
use super::{ComponentData, bitplane, build, idwt, mct, segment, tile};
use crate::cancel::Cancel;
use crate::error::{DecodeError, Result, bail};
use crate::j2c::segment::MAX_BITPLANE_COUNT;
use crate::math::{self, SimdBuffer};
use crate::reader::BitReader;

pub(crate) fn decode(
    data: &[u8],
    header: &Header,
    ctx: &mut DecoderContext,
    cancel: Cancel<'_>,
) -> Result<()> {
    let mut reader = BitReader::new(data);
    let tile_count = tile::parse(&mut reader, header, &mut ctx.tiles)?;
    if tile_count == 0 {
        bail!(DecodeError::Malformed);
    }

    let first_tile = ctx.tiles.first().ok_or(DecodeError::Malformed)?;
    let first_mct = first_tile.mct;
    let num_components = first_tile.component_infos(header).len();
    let zero_channels = tile_count != 1
        || first_tile.tile_parts.is_empty()
        || !first_tile
            .component_infos(header)
            .iter()
            .all(|ci| is_full_image_tile(first_tile, ci, header));

    ctx.reset(header, num_components, zero_channels, cancel)?;

    let decoded_resolutions = usize::from(header.levels - header.skipped_resolution_levels) + 1;
    ctx.layers_received.clear();
    ctx.layers_received.resize(decoded_resolutions, u8::MAX);

    let tiles = ctx.tiles.get(..tile_count).ok_or(DecodeError::Malformed)?;
    for tile in tiles {
        cancel.check()?;

        if tile.tile_parts.is_empty() {
            ctx.layers_received.fill(0);
            continue;
        }

        let num_layers = usize::from(tile.num_layers);
        ctx.received_packets.clear();
        ctx.received_packets
            .resize(decoded_resolutions * num_layers, 0);

        let progression_iterator = progression_iterator(tile, header)?;
        let channel_data = ctx
            .channel_data
            .get_mut(..num_components)
            .ok_or(DecodeError::Malformed)?;

        decode_tile(
            data,
            tile,
            header,
            progression_iterator,
            &mut ctx.tile_decode_context,
            channel_data,
            &mut ctx.storage,
            &mut ctx.received_packets,
            decoded_resolutions,
            cancel,
        )?;

        update_layers_received(
            tile,
            header,
            decoded_resolutions,
            &ctx.received_packets,
            &mut ctx.expected_packets,
            &mut ctx.layers_received,
        )?;
    }

    let channel_data = ctx
        .channel_data
        .get_mut(..num_components)
        .ok_or(DecodeError::Malformed)?;

    if first_mct {
        let first_tile = ctx.tiles.first().ok_or(DecodeError::Malformed)?;
        mct::apply_inverse(
            channel_data,
            first_tile.component_infos(header),
            header,
            cancel,
        )?;
    }

    apply_sign_shift(channel_data, cancel)?;

    Ok(())
}

fn update_layers_received(
    tile: &Tile,
    header: &Header,
    decoded_resolutions: usize,
    received_packets: &[u64],
    expected_packets: &mut Vec<u64>,
    layers_received: &mut [u8],
) -> Result<()> {
    let num_layers = usize::from(tile.num_layers);
    let effective_layers = header
        .max_layers
        .map_or(tile.num_layers, |m| m.min(tile.num_layers));

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

#[derive(Default)]
pub struct DecoderContext {
    tiles: Vec<Tile>,
    tile_decode_context: TileDecodeContext,
    pub(crate) channel_data: Vec<ComponentData>,
    pub(crate) layers_received: Vec<u8>,
    received_packets: Vec<u64>,
    expected_packets: Vec<u64>,
    storage: DecompositionStorage,
}

impl DecoderContext {
    fn reset(
        &mut self,
        header: &Header,
        num_components: usize,
        zero_channels: bool,
        cancel: Cancel<'_>,
    ) -> Result<()> {
        self.tile_decode_context.reset();
        self.storage.reset();

        let sample_count = (header.size_data.image_width() as usize)
            .checked_mul(header.size_data.image_height() as usize)
            .ok_or(DecodeError::TooLarge)?;
        if self.channel_data.len() < num_components {
            self.channel_data
                .resize_with(num_components, || ComponentData {
                    container: SimdBuffer::default(),
                });
        }
        for channel in self
            .channel_data
            .get_mut(..num_components)
            .ok_or(DecodeError::Malformed)?
        {
            cancel.check()?;

            channel
                .container
                .prepare(sample_count, zero_channels, cancel)?;
        }

        Ok(())
    }
}

fn is_full_image_tile(tile: &Tile, component_info: &ComponentInfo, header: &Header) -> bool {
    let component_tile = ComponentTile::new(tile, component_info);
    let resolution_tile = ResolutionTile::new(
        component_tile,
        component_info.num_resolution_levels() - 1 - header.skipped_resolution_levels,
    );
    resolution_tile.rect.x0 == 0
        && resolution_tile.rect.y0 == 0
        && resolution_tile.rect.width() == header.size_data.image_width()
        && resolution_tile.rect.height() == header.size_data.image_height()
}

fn decode_tile(
    codestream: &[u8],
    tile: &Tile,
    header: &Header,
    progression_iterator: ProgressionIterator<'_>,
    tile_ctx: &mut TileDecodeContext,
    channel_data: &mut [ComponentData],
    storage: &mut DecompositionStorage,
    received_packets: &mut [u64],
    decoded_resolutions: usize,
    cancel: Cancel<'_>,
) -> Result<()> {
    storage.reset();

    build::build(
        tile,
        header,
        storage,
        header.skipped_resolution_levels,
        cancel,
    )?;
    segment::parse(
        codestream,
        tile,
        progression_iterator,
        header,
        storage,
        received_packets,
        decoded_resolutions,
        cancel,
    )?;

    for ((idx, component_info), channel) in tile
        .component_infos(header)
        .iter()
        .enumerate()
        .zip(channel_data.iter_mut())
    {
        cancel.check()?;

        let full_image_tile = is_full_image_tile(tile, component_info, header);

        if full_image_tile {
            idwt::apply(
                codestream,
                storage,
                &mut tile_ctx.idwt_scratch,
                &mut tile_ctx.bit_plane_decode_context,
                &mut tile_ctx.bit_plane_decode_buffers,
                channel.container.as_vec_mut(),
                idx,
                component_info,
                header,
                cancel,
            )?;
            channel.container.restore_padding();
        } else {
            tile_ctx.idwt_output.rect = idwt::apply(
                codestream,
                storage,
                &mut tile_ctx.idwt_scratch,
                &mut tile_ctx.bit_plane_decode_context,
                &mut tile_ctx.bit_plane_decode_buffers,
                &mut tile_ctx.idwt_output.coefficients,
                idx,
                component_info,
                header,
                cancel,
            )?;
            store(tile, header, tile_ctx, channel, component_info, cancel)?;
        }
    }

    Ok(())
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

#[derive(Default)]
pub(crate) struct TileDecodeContext {
    pub(crate) idwt_output: IDWTOutput,
    pub(crate) idwt_scratch: IdwtScratch,
    pub(crate) bit_plane_decode_context: BitPlaneDecodeContext,
    pub(crate) bit_plane_decode_buffers: BitPlaneDecodeBuffers,
}

impl TileDecodeContext {
    fn reset(&mut self) {}
}

pub(crate) fn decode_sub_band_strip(
    codestream: &[u8],
    sub_band_idx: usize,
    resolution: u8,
    component_info: &ComponentInfo,
    bit_plane_ctx: &mut BitPlaneDecodeContext,
    bit_plane_buffers: &mut BitPlaneDecodeBuffers,
    storage: &DecompositionStorage,
    header: &Header,
    abs_y0: u32,
    abs_y1: u32,
    dst: &mut Vec<f32>,
    cancel: Cancel<'_>,
) -> Result<()> {
    let sub_band = storage
        .sub_bands
        .get(sub_band_idx)
        .ok_or(DecodeError::Malformed)?;

    let sb_width = sub_band.rect.width() as usize;
    let strip_height = abs_y1.saturating_sub(abs_y0) as usize;
    let strip_len = sb_width
        .checked_mul(strip_height)
        .ok_or(DecodeError::TooLarge)?;
    cancel.ensure_len(dst, strip_len)?;

    if strip_len == 0 {
        return Ok(());
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

            cancel.check()?;

            let x_offset = code_block.rect.x0 - sub_band.rect.x0;
            let y_offset = code_block.rect.y0 - abs_y0;
            let mut base_idx = (y_offset as usize) * sb_width + x_offset as usize;

            if !bitplane::decode(
                codestream,
                code_block,
                sub_band.sub_band_type,
                num_bitplanes,
                &component_info.coding_style.parameters.code_block_style,
                bit_plane_ctx,
                bit_plane_buffers,
                storage,
                header.max_layers,
                header.strict,
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

            bit_plane_ctx.write_reconstructed(
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

fn apply_sign_shift(channel_data: &mut [ComponentData], cancel: Cancel<'_>) -> Result<()> {
    use crate::math::{Level, dispatch, f32x8};

    for channel in channel_data.iter_mut() {
        cancel.check()?;

        dispatch!(Level::new(), simd => {
            let offset_v = f32x8::splat(simd, 128.0);
            for chunk in channel.container.as_chunks_mut::<8>().0 {
                let v = f32x8::from_slice(simd, chunk);
                (v + offset_v).store(chunk);
            }
        });
    }

    Ok(())
}

fn store(
    tile: &Tile,
    header: &Header,
    tile_ctx: &mut TileDecodeContext,
    channel_data: &mut ComponentData,
    component_info: &ComponentInfo,
    cancel: Cancel<'_>,
) -> Result<()> {
    let idwt_output = &mut tile_ctx.idwt_output;

    let component_tile = ComponentTile::new(tile, component_info);
    let resolution_tile = ResolutionTile::new(
        component_tile,
        component_info.num_resolution_levels() - 1 - header.skipped_resolution_levels,
    );

    if idwt_output.rect.width() == 0 || idwt_output.rect.height() == 0 {
        return Ok(());
    }

    let input_row_iter = idwt_output
        .coefficients
        .chunks_exact(idwt_output.rect.width() as usize)
        .take(idwt_output.rect.height() as usize);

    let output_row_iter = channel_data
        .container
        .chunks_exact_mut(header.size_data.image_width() as usize)
        .skip(resolution_tile.rect.y0 as usize);

    for (input_row, output_row) in input_row_iter.zip(output_row_iter) {
        cancel.check()?;

        let output_row = output_row
            .get_mut(resolution_tile.rect.x0 as usize..)
            .and_then(|r| r.get_mut(..input_row.len()))
            .ok_or(DecodeError::Malformed)?;
        output_row.copy_from_slice(input_row);
    }

    Ok(())
}
