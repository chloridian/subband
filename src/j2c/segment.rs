use core::result;

use super::build::{Layer, Segment, can_stop_at_discard, max_decoded_resolutions};
use super::codestream::markers::{EOC, EPH, SOP};
use super::codestream::{ComponentInfo, Header};
use super::decode::{DecompositionStorage, SubBandIter};
use super::progression::{ProgressionData, ProgressionIterator};
use super::tile::{Tile, TilePart};
use crate::check_cancelled;
use crate::error::{DecodeError, Result, bail};
use crate::reader::{BitReadError, BitReader};

pub(crate) const MAX_BITPLANE_COUNT: u8 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PacketError {
    Done,
    Truncated,
    Malformed,
}

impl From<BitReadError> for PacketError {
    fn from(err: BitReadError) -> Self {
        match err {
            BitReadError::Eof => Self::Truncated,
            BitReadError::InvalidStuffBit => Self::Malformed,
        }
    }
}

pub(crate) fn parse(
    codestream: &[u8],
    tile: &Tile,
    mut progression_iterator: ProgressionIterator<'_>,
    header: &Header,
    skipped_resolution_levels: u8,
    storage: &mut DecompositionStorage,
    received_packets: &mut [u64],
    decoded_resolutions: usize,
    cancelled: &dyn Fn() -> bool,
) -> Result<()> {
    let num_layers = usize::from(tile.num_layers);
    let component_infos = tile.component_infos(header);
    let stop_at_discard = can_stop_at_discard(tile, header);
    let max_res = max_decoded_resolutions(tile, header, skipped_resolution_levels)?;

    for tile_part_data in &tile.tile_parts {
        let tile_part = tile_part_data
            .to_tile_part(codestream, header)
            .ok_or(DecodeError::Malformed)?;
        match parse_inner(
            tile_part,
            &mut progression_iterator,
            component_infos,
            storage,
            received_packets,
            decoded_resolutions,
            num_layers,
            skipped_resolution_levels,
            stop_at_discard,
            max_res,
            cancelled,
        )? {
            Ok(()) | Err(PacketError::Truncated) => {}
            Err(PacketError::Done) => break,
            Err(PacketError::Malformed) if header.strict => bail!(DecodeError::Malformed),
            Err(PacketError::Malformed) => {}
        }
    }

    Ok(())
}

fn parse_inner(
    mut tile_part: TilePart<'_>,
    progression_iterator: &mut ProgressionIterator<'_>,
    component_infos: &[ComponentInfo],
    storage: &mut DecompositionStorage,
    received_packets: &mut [u64],
    decoded_resolutions: usize,
    num_layers: usize,
    skipped_resolution_levels: u8,
    stop_at_discard: bool,
    max_res: u8,
    cancelled: &dyn Fn() -> bool,
) -> Result<result::Result<(), PacketError>> {
    while !tile_part.header().at_end() {
        check_cancelled(cancelled)?;

        match parse_packet(
            &mut tile_part,
            progression_iterator,
            component_infos,
            storage,
            skipped_resolution_levels,
            stop_at_discard,
            max_res,
        ) {
            Ok(prog) => {
                let res = usize::from(prog.resolution);
                let layer = usize::from(prog.layer_num);
                if res < decoded_resolutions
                    && let Some(slot) = received_packets.get_mut(res * num_layers + layer)
                {
                    *slot = slot.saturating_add(1);
                }
            }
            Err(err) => return Ok(Err(err)),
        }
    }

    Ok(Ok(()))
}

fn parse_packet(
    tile_part: &mut TilePart<'_>,
    progression_iterator: &mut ProgressionIterator<'_>,
    component_infos: &[ComponentInfo],
    storage: &mut DecompositionStorage,
    skipped_resolution_levels: u8,
    stop_at_discard: bool,
    max_res: u8,
) -> result::Result<ProgressionData, PacketError> {
    let Some(progression_data) = progression_iterator.next() else {
        let header_reader = tile_part.header();
        if header_reader.peek_marker() == Some(EOC) || header_reader.tail() == Some(&[0xFF]) {
            return Err(PacketError::Done);
        }
        return Err(PacketError::Malformed);
    };
    let resolution = progression_data.resolution;
    if stop_at_discard && resolution >= max_res {
        return Err(PacketError::Done);
    }
    let component_info = component_infos
        .get(progression_data.component as usize)
        .ok_or(PacketError::Malformed)?;
    let comp_decoded_resolutions =
        component_info.num_resolution_levels() - skipped_resolution_levels;
    let discard_packet = resolution >= comp_decoded_resolutions;
    let tile_decompositions = storage
        .tile_decompositions
        .get_mut(progression_data.component as usize)
        .ok_or(PacketError::Malformed)?;
    let sub_band_iter = tile_decompositions.sub_band_iter(resolution, &storage.decompositions);

    let body_reader = tile_part.body();

    if component_info.coding_style.flags.may_use_sop_markers() {
        if body_reader.peek_marker() == Some(SOP) {
            body_reader
                .read_marker()
                .map_err(|_| PacketError::Truncated)?;
            body_reader.skip_bytes(4).ok_or(PacketError::Truncated)?;
        } else if body_reader.tail() == Some(&[0xFF]) {
            return Err(PacketError::Truncated);
        }
    }

    let header_reader = tile_part.header();

    let zero_length = header_reader.read_bits_with_stuffing(1)? == 0;
    let mut skip_body_bytes = 0_usize;

    if !zero_length {
        for sub_band in sub_band_iter.clone() {
            if discard_packet {
                let bytes = resolve_discarded_segments(
                    sub_band,
                    &progression_data,
                    header_reader,
                    storage,
                    component_info,
                )?;
                skip_body_bytes = skip_body_bytes
                    .checked_add(bytes)
                    .ok_or(PacketError::Malformed)?;
            } else if let Err(err) = resolve_segments(
                sub_band,
                &progression_data,
                header_reader,
                storage,
                component_info,
            ) {
                rollback_packet_header(sub_band_iter, &progression_data, storage);
                return Err(err);
            }
        }
    }

    header_reader.align();

    if component_info.coding_style.flags.uses_eph_marker() {
        let tail = header_reader.tail().unwrap_or(&[]);
        if tail.is_empty() || tail == [0xFF] {
            if !zero_length && !discard_packet {
                rollback_packet_header(sub_band_iter, &progression_data, storage);
            }
            return Err(PacketError::Truncated);
        }
        if header_reader.read_marker().ok() != Some(EPH) {
            if !zero_length && !discard_packet {
                rollback_packet_header(sub_band_iter, &progression_data, storage);
            }
            return Err(PacketError::Malformed);
        }
    }

    let body_offset = tile_part.body_offset();
    let body_reader = tile_part.body();

    if !zero_length {
        if discard_packet {
            body_reader
                .skip_bytes(skip_body_bytes)
                .ok_or(PacketError::Truncated)?;
            return Ok(progression_data);
        }
        let layer_num = progression_data.layer_num as usize;
        let precinct_idx = progression_data.precinct as usize;
        let mut truncated = false;

        for sub_band in sub_band_iter {
            let cb_range = storage
                .sub_bands
                .get(sub_band)
                .and_then(|sb| storage.precincts.get(sb.precincts.clone()))
                .and_then(|precincts| precincts.get(precinct_idx))
                .map(|p| p.code_blocks.clone())
                .ok_or(PacketError::Malformed)?;

            for cb_idx in cb_range {
                if truncated {
                    rollback_code_block(cb_idx, layer_num, storage);
                    continue;
                }

                let seg_range = storage
                    .code_blocks
                    .get(cb_idx)
                    .and_then(|cb| storage.layers.get(cb.layers_range()))
                    .and_then(|layers| layers.get(layer_num))
                    .and_then(|layer| layer.segments());

                if let Some(seg_range) = seg_range {
                    let segments = storage
                        .segments
                        .get_mut(seg_range)
                        .ok_or(PacketError::Malformed)?;

                    let mut complete = true;
                    for segment in segments {
                        let start = body_offset
                            .checked_add(body_reader.offset())
                            .ok_or(PacketError::Malformed)?;
                        if let Some(bytes) = body_reader.read_bytes(segment.data_length as usize) {
                            start
                                .checked_add(bytes.len())
                                .ok_or(PacketError::Malformed)?;
                            segment.data_start = start;
                        } else {
                            complete = false;
                            break;
                        }
                    }

                    if !complete {
                        truncated = true;
                        rollback_code_block(cb_idx, layer_num, storage);
                    }
                }
            }
        }

        if truncated {
            return Err(PacketError::Truncated);
        }
    }

    Ok(progression_data)
}

fn rollback_packet_header(
    sub_band_iter: SubBandIter,
    progression_data: &ProgressionData,
    storage: &mut DecompositionStorage,
) {
    let layer_num = progression_data.layer_num as usize;
    let precinct_idx = progression_data.precinct as usize;

    for sub_band in sub_band_iter {
        let Some(cb_range) = storage
            .sub_bands
            .get(sub_band)
            .and_then(|sb| storage.precincts.get(sb.precincts.clone()))
            .and_then(|precincts| precincts.get(precinct_idx))
            .map(|p| p.code_blocks.clone())
        else {
            continue;
        };

        for cb_idx in cb_range {
            rollback_code_block(cb_idx, layer_num, storage);
        }
    }
}

fn rollback_code_block(cb_idx: usize, layer_num: usize, storage: &mut DecompositionStorage) {
    let Some(code_block) = storage.code_blocks.get_mut(cb_idx) else {
        return;
    };
    let Some(seg_range) = storage
        .layers
        .get_mut(code_block.layers_range())
        .and_then(|layers| layers.get_mut(layer_num))
        .and_then(Layer::take)
    else {
        return;
    };

    let passes: u8 = storage
        .segments
        .get(seg_range)
        .map_or(0, |segs| segs.iter().map(|s| s.coding_pases).sum());
    code_block.number_of_coding_passes = code_block.number_of_coding_passes.saturating_sub(passes);
    if code_block.number_of_coding_passes == 0 {
        code_block.missing_bit_planes = 0;
    }
}

fn read_added_coding_passes(reader: &mut BitReader<'_>) -> result::Result<u8, PacketError> {
    if reader.read_bits_with_stuffing(1)? == 0 {
        return Ok(1);
    }
    if reader.read_bits_with_stuffing(1)? == 0 {
        return Ok(2);
    }
    let next = reader.read_bits_with_stuffing(2)?;
    if next < 3 {
        return Ok(3 + next as u8);
    }
    let ext = reader.read_bits_with_stuffing(5)?;
    if ext < 31 {
        return Ok(6 + ext as u8);
    }
    let tail = reader.read_bits_with_stuffing(7)?;
    Ok(37 + tail as u8)
}

fn resolve_segments(
    sub_band_dx: usize,
    progression_data: &ProgressionData,
    reader: &mut BitReader<'_>,
    storage: &mut DecompositionStorage,
    component_info: &ComponentInfo,
) -> result::Result<(), PacketError> {
    const MAX_CODING_PASSES: u8 = 1 + 3 * (MAX_BITPLANE_COUNT - 1);

    let sub_band = storage
        .sub_bands
        .get(sub_band_dx)
        .ok_or(PacketError::Malformed)?;
    let precincts = storage
        .precincts
        .get_mut(sub_band.precincts.clone())
        .ok_or(PacketError::Malformed)?;
    let precinct = precincts
        .get_mut(progression_data.precinct as usize)
        .ok_or(PacketError::Malformed)?;
    let code_blocks = storage
        .code_blocks
        .get_mut(precinct.code_blocks.clone())
        .ok_or(PacketError::Malformed)?;

    for code_block in code_blocks {
        let is_included = if code_block.has_been_included {
            reader.read_bits_with_stuffing(1)? == 1
        } else {
            precinct.code_inclusion_tree.read(
                code_block.x_idx,
                code_block.y_idx,
                reader,
                progression_data.layer_num as u32 + 1,
                &mut storage.tag_tree_nodes,
            )? <= progression_data.layer_num as u32
        };

        if !is_included {
            continue;
        }

        let layer = storage
            .layers
            .get_mut(code_block.layers_range())
            .and_then(|layers| layers.get_mut(progression_data.layer_num as usize))
            .ok_or(PacketError::Malformed)?;

        let included_first_time = is_included && !code_block.has_been_included;

        if included_first_time {
            code_block.missing_bit_planes = precinct.zero_bitplane_tree.read(
                code_block.x_idx,
                code_block.y_idx,
                reader,
                u32::MAX,
                &mut storage.tag_tree_nodes,
            )? as u8;
        }

        code_block.has_been_included |= is_included;

        let added_coding_passes = read_added_coding_passes(reader)?;

        let mut k = 0_u32;

        while reader.read_bits_with_stuffing(1)? == 1 {
            k = k.checked_add(1).ok_or(PacketError::Malformed)?;
        }

        code_block.l_block = code_block
            .l_block
            .checked_add(k)
            .ok_or(PacketError::Malformed)?;

        let previous_layers_passes = code_block.number_of_coding_passes;
        let cumulative_passes = previous_layers_passes
            .checked_add(added_coding_passes)
            .ok_or(PacketError::Malformed)?;

        if cumulative_passes > MAX_CODING_PASSES {
            return Err(PacketError::Malformed);
        }

        let get_segment_idx = |pass_idx: u8| {
            if component_info.code_block_style().termination_on_each_pass {
                pass_idx
            } else if component_info
                .code_block_style()
                .selective_arithmetic_coding_bypass
            {
                segment_idx_for_bypass(pass_idx)
            } else {
                code_block.non_empty_layer_count
            }
        };

        let start = storage.segments.len();

        let mut push_segment = |segment: u8, coding_passes_for_segment: u8| {
            assert!(coding_passes_for_segment > 0);

            let length_bits = code_block
                .l_block
                .checked_add(coding_passes_for_segment.ilog2())
                .ok_or(PacketError::Malformed)?;
            if length_bits > 32 {
                return Err(PacketError::Malformed);
            }
            let length = reader.read_bits_with_stuffing(length_bits as u8)?;

            storage.segments.push(Segment {
                idx: segment,
                data_length: length,
                coding_pases: coding_passes_for_segment,
                data_start: 0,
            });

            Ok(())
        };

        let mut last_segment = get_segment_idx(previous_layers_passes);
        let mut coding_passes_for_segment = 0;

        for coding_pass in previous_layers_passes..cumulative_passes {
            let segment = get_segment_idx(coding_pass);

            if segment != last_segment {
                push_segment(last_segment, coding_passes_for_segment)?;
                last_segment = segment;
                coding_passes_for_segment = 1;
            } else {
                coding_passes_for_segment += 1;
            }
        }

        if coding_passes_for_segment > 0 {
            push_segment(last_segment, coding_passes_for_segment)?;
        }

        let start = u32::try_from(start).map_err(|_| PacketError::Malformed)?;
        let end = u32::try_from(storage.segments.len()).map_err(|_| PacketError::Malformed)?;
        *layer = Layer { start, end };
        code_block.number_of_coding_passes += added_coding_passes;
        code_block.non_empty_layer_count += 1;
    }

    Ok(())
}

fn resolve_discarded_segments(
    sub_band_dx: usize,
    progression_data: &ProgressionData,
    reader: &mut BitReader<'_>,
    storage: &mut DecompositionStorage,
    component_info: &ComponentInfo,
) -> result::Result<usize, PacketError> {
    const MAX_CODING_PASSES: u8 = 1 + 3 * (MAX_BITPLANE_COUNT - 1);

    let sub_band = storage
        .sub_bands
        .get(sub_band_dx)
        .ok_or(PacketError::Malformed)?;
    let precincts = storage
        .precincts
        .get_mut(sub_band.precincts.clone())
        .ok_or(PacketError::Malformed)?;
    let precinct = precincts
        .get_mut(progression_data.precinct as usize)
        .ok_or(PacketError::Malformed)?;
    let code_blocks = storage
        .code_blocks
        .get_mut(precinct.code_blocks.clone())
        .ok_or(PacketError::Malformed)?;

    let mut total_bytes = 0_usize;

    for code_block in code_blocks {
        let is_included = if code_block.has_been_included {
            reader.read_bits_with_stuffing(1)? == 1
        } else {
            precinct.code_inclusion_tree.read(
                code_block.x_idx,
                code_block.y_idx,
                reader,
                progression_data.layer_num as u32 + 1,
                &mut storage.tag_tree_nodes,
            )? <= progression_data.layer_num as u32
        };

        if !is_included {
            continue;
        }

        if !code_block.has_been_included {
            code_block.missing_bit_planes = precinct.zero_bitplane_tree.read(
                code_block.x_idx,
                code_block.y_idx,
                reader,
                u32::MAX,
                &mut storage.tag_tree_nodes,
            )? as u8;
        }

        code_block.has_been_included = true;

        let added_coding_passes = read_added_coding_passes(reader)?;

        let mut k = 0_u32;
        while reader.read_bits_with_stuffing(1)? == 1 {
            k = k.checked_add(1).ok_or(PacketError::Malformed)?;
        }

        code_block.l_block = code_block
            .l_block
            .checked_add(k)
            .ok_or(PacketError::Malformed)?;

        let previous_layers_passes = code_block.number_of_coding_passes;
        let cumulative_passes = previous_layers_passes
            .checked_add(added_coding_passes)
            .ok_or(PacketError::Malformed)?;

        if cumulative_passes > MAX_CODING_PASSES {
            return Err(PacketError::Malformed);
        }

        let get_segment_idx = |pass_idx: u8| {
            if component_info.code_block_style().termination_on_each_pass {
                pass_idx
            } else if component_info
                .code_block_style()
                .selective_arithmetic_coding_bypass
            {
                segment_idx_for_bypass(pass_idx)
            } else {
                code_block.non_empty_layer_count
            }
        };

        let mut read_length = |coding_passes_for_segment: u8| -> result::Result<(), PacketError> {
            assert!(coding_passes_for_segment > 0);
            let length_bits = code_block
                .l_block
                .checked_add(coding_passes_for_segment.ilog2())
                .ok_or(PacketError::Malformed)?;
            if length_bits > 32 {
                return Err(PacketError::Malformed);
            }
            let length = reader.read_bits_with_stuffing(length_bits as u8)?;
            total_bytes = total_bytes
                .checked_add(length as usize)
                .ok_or(PacketError::Malformed)?;
            Ok(())
        };

        let mut last_segment = get_segment_idx(previous_layers_passes);
        let mut coding_passes_for_segment = 0;

        for coding_pass in previous_layers_passes..cumulative_passes {
            let segment = get_segment_idx(coding_pass);
            if segment != last_segment {
                read_length(coding_passes_for_segment)?;
                last_segment = segment;
                coding_passes_for_segment = 1;
            } else {
                coding_passes_for_segment += 1;
            }
        }

        if coding_passes_for_segment > 0 {
            read_length(coding_passes_for_segment)?;
        }

        code_block.number_of_coding_passes += added_coding_passes;
        code_block.non_empty_layer_count += 1;
    }

    Ok(total_bytes)
}

pub(crate) fn segment_idx_for_bypass(pass_idx: u8) -> u8 {
    if pass_idx < 10 {
        0
    } else {
        1 + (2 * ((pass_idx - 10) / 3)) + (if ((pass_idx - 10) % 3) == 2 { 1 } else { 0 })
    }
}
