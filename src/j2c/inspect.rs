use core::ops::Range;
use std::vec;
use std::vec::Vec;

use super::build::SubBandType;
use super::codestream::{
    self, ComponentInfo, Header, MarkerBudget, markers, skip_marker_segment,
    validate_resource_limits,
};
use super::progression::{ProgressionData, progression_iterator};
use super::rect::IntRect;
use super::segment::{MAX_BITPLANE_COUNT, segment_idx_for_bypass};
use super::tag_tree::{TagNode, TagTree};
use super::tile::{
    ComponentTile, MAX_TILE_PARTS_PER_TILE, MergedTilePart, ResolutionTile, SeparatedTilePart,
    Tile, TilePart, ppt_marker, sot_marker,
};
use crate::DecodeSettings;
use crate::cancel::Cancel;
use crate::error::{DecodeError, Result, bail};
use crate::reader::BitReader;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspection {
    width: u32,
    height: u32,
    components: u8,
    levels: u8,
    layers: u8,
    table_layers: u8,
    complete_discard: Option<u8>,
    buckets: Vec<Bucket>,
}

impl Inspection {
    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn components(&self) -> u8 {
        self.components
    }

    pub fn levels(&self) -> u8 {
        self.levels
    }

    pub fn layers(&self) -> u8 {
        self.layers
    }

    pub fn complete_discard(&self) -> Option<u8> {
        self.complete_discard
    }

    pub fn bytes_for(&self, discard: u8, layers: Option<u8>) -> Option<usize> {
        let d = usize::from(discard.min(self.levels));
        let num_d = usize::from(self.levels) + 1;
        let l_count =
            usize::from(layers.map_or(self.table_layers, |n| n.max(1).min(self.table_layers)));
        let stride = usize::from(self.table_layers);

        let mut max_end = 0_usize;
        for b in d..num_d {
            for l in 0..l_count {
                let bucket = self.buckets.get(b * stride + l)?;
                if bucket.known < bucket.expected {
                    return None;
                }
                max_end = max_end.max(bucket.max_end);
            }
        }

        Some(max_end)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Bucket {
    expected: u64,
    known: u64,
    present: u64,
    max_end: usize,
}

struct InspectTilePart<'a> {
    part: TilePart<'a>,
    sod_end: usize,
    declared_end: Option<usize>,
    plt_markers: Vec<PltMarkerData<'a>>,
}

struct PltMarkerData<'a> {
    sequence_idx: u8,
    data: &'a [u8],
}

#[derive(Default)]
struct HeaderState {
    code_blocks: Vec<InspectCodeBlock>,
    precincts: Vec<InspectPrecinct>,
    tag_tree_nodes: Vec<TagNode>,
    sub_bands: Vec<Range<usize>>,
    decompositions: Vec<[usize; 3]>,
    components: Vec<ComponentSubBands>,
}

struct ComponentSubBands {
    first_ll: usize,
    decompositions: Range<usize>,
}

struct InspectPrecinct {
    code_blocks: Range<usize>,
    code_inclusion_tree: TagTree,
    zero_bitplane_tree: TagTree,
}

struct InspectCodeBlock {
    x_idx: u32,
    y_idx: u32,
    has_been_included: bool,
    l_block: u32,
    number_of_coding_passes: u8,
    non_empty_layer_count: u8,
}

pub(crate) fn inspect(stream: &[u8], settings: &DecodeSettings) -> Result<Inspection> {
    let cancel = Cancel(settings.cancel.as_ref());
    cancel.check()?;

    let mut reader = BitReader::new(stream);
    if reader.read_marker()? != markers::SOC {
        bail!(DecodeError::Malformed);
    }

    let main_header = codestream::read_header(&mut reader, settings)?;
    let tile_count = main_header.size_data.num_tiles()?;
    let mut tiles: Vec<Tile<'_>> = (0..tile_count)
        .map(|idx| Tile::new(idx, &main_header))
        .collect();
    let mut tile_parts: Vec<Vec<InspectTilePart<'_>>> =
        (0..tile_count).map(|_| Vec::new()).collect();

    let mut marker_budget = main_header.marker_budget;
    let mut tile_part_idx = 0_usize;

    while reader.peek_marker() == Some(markers::SOT) {
        cancel.check()?;
        if !parse_tile_part(
            &mut reader,
            &main_header,
            &mut tiles,
            &mut tile_parts,
            tile_part_idx,
            &mut marker_budget,
        )? {
            break;
        }
        tile_part_idx += 1;
    }

    let levels = tiles
        .iter()
        .flat_map(|t| {
            t.component_infos
                .iter()
                .map(|c| c.num_decomposition_levels())
        })
        .min()
        .unwrap_or(main_header.levels);
    let table_layers = tiles
        .iter()
        .map(|t| t.num_layers)
        .max()
        .unwrap_or(main_header.global_coding_style.num_layers);

    let num_d = usize::from(levels) + 1;
    let stride = usize::from(table_layers);
    let mut buckets = vec![Bucket::default(); num_d * stride];

    for tile in &tiles {
        cancel.check()?;
        count_expected(tile, &main_header, levels, stride, &mut buckets)?;
    }

    let mut state = HeaderState::default();
    let data_len = stream.len();

    for (tile, parts) in tiles.iter().zip(tile_parts.iter_mut()) {
        cancel.check()?;
        if parts.is_empty() {
            continue;
        }

        let mut prog = progression_iterator(tile, &main_header)?;
        let use_plt = parts.iter().all(|tp| !tp.plt_markers.is_empty());

        if use_plt {
            for tp in parts.iter_mut() {
                cancel.check()?;
                tp.plt_markers.sort_by_key(|m| m.sequence_idx);
                let lengths = parse_plt_lengths(&tp.plt_markers)?;
                let mut cursor = tp.sod_end;

                for pkt_len in lengths {
                    let Some(prog_data) = prog.next() else {
                        break;
                    };
                    let pkt_end = cursor.checked_add(pkt_len).ok_or(DecodeError::Malformed)?;
                    let present =
                        pkt_end <= data_len && tp.declared_end.is_none_or(|end| pkt_end <= end);
                    record_packet(
                        &tile.component_infos,
                        prog_data,
                        pkt_end,
                        present,
                        levels,
                        stride,
                        &mut buckets,
                    );
                    cursor = pkt_end;
                }
            }
        } else {
            build_header_state(tile, &mut state, cancel)?;
            for tp in parts.iter_mut() {
                cancel.check()?;
                parse_tile_part_packets(
                    tp,
                    &mut prog,
                    &tile.component_infos,
                    &mut state,
                    data_len,
                    levels,
                    stride,
                    &mut buckets,
                    cancel,
                )?;
            }
        }
    }

    let complete_layers = usize::from(
        main_header
            .max_layers
            .map_or(table_layers, |n| n.min(table_layers)),
    );
    let mut complete_discard = None;
    for d in 0..num_d {
        let mut all_present = true;
        'check: for b in d..num_d {
            for l in 0..complete_layers {
                if let Some(bucket) = buckets.get(b * stride + l)
                    && bucket.present < bucket.expected
                {
                    all_present = false;
                    break 'check;
                }
            }
        }
        if all_present {
            complete_discard = Some(d as u8);
            break;
        }
    }

    Ok(Inspection {
        width: main_header.size_data.reference_grid_width,
        height: main_header.size_data.reference_grid_height,
        components: main_header.size_data.num_components as u8,
        levels,
        layers: main_header.global_coding_style.num_layers,
        table_layers,
        complete_discard,
        buckets,
    })
}

fn parse_tile_part<'a>(
    reader: &mut BitReader<'a>,
    main_header: &'a Header,
    tiles: &mut [Tile<'a>],
    tile_parts: &mut [Vec<InspectTilePart<'a>>],
    tile_part_idx: usize,
    marker_budget: &mut MarkerBudget,
) -> Result<bool> {
    if reader.peek_bytes(12).is_none() {
        return Ok(false);
    }

    if reader.read_marker()? != markers::SOT {
        return Ok(false);
    }

    let sot_start = reader.offset();
    let Some(tile_part_header) = sot_marker(reader) else {
        return Ok(false);
    };
    marker_budget.record(reader.offset() - sot_start)?;

    let tile_index = usize::from(tile_part_header.tile_index);
    if tile_index >= tiles.len() {
        bail!(DecodeError::Malformed);
    }

    let sot_end = reader.offset();
    let declared_end = if tile_part_header.tile_part_length == 0 {
        None
    } else {
        let len = (tile_part_header.tile_part_length as usize)
            .checked_sub(12)
            .ok_or(DecodeError::Malformed)?;
        Some(sot_end.checked_add(len).ok_or(DecodeError::Malformed)?)
    };

    let tile = tiles.get_mut(tile_index).ok_or(DecodeError::Malformed)?;
    let parts = tile_parts
        .get_mut(tile_index)
        .ok_or(DecodeError::Malformed)?;
    if parts.len() >= MAX_TILE_PARTS_PER_TILE {
        bail!(DecodeError::TooLarge);
    }

    let mut ppt_headers = vec![];
    let mut plt_markers = vec![];
    let mut coding_style_changed = false;
    let mut saw_sod = false;

    loop {
        if let Some(end) = declared_end
            && reader.offset() >= end
        {
            break;
        }

        let Some(marker) = reader.peek_marker() else {
            return Ok(false);
        };

        if marker == markers::EOC {
            break;
        }

        if marker == markers::SOD {
            reader.read_marker()?;
            marker_budget.record(0)?;
            saw_sod = true;
            break;
        }

        if (0x30..=0x3F).contains(&marker) {
            reader.read_marker()?;
            marker_budget.record(0)?;
            continue;
        }

        let Some(&[_, _, hi, lo]) = reader.peek_bytes(4).and_then(|b| b.first_chunk::<4>()) else {
            return Ok(false);
        };
        let seg_len = u16::from_be_bytes([hi, lo]);
        if seg_len < 2 {
            bail!(DecodeError::Malformed);
        }
        if reader.peek_bytes(2 + usize::from(seg_len)).is_none() {
            return Ok(false);
        }

        reader.read_marker()?;
        let seg_start = reader.offset();

        match marker {
            markers::COD => {
                let cod = codestream::cod_marker(reader)?;
                tile.mct = cod.mct;
                tile.num_layers = cod.num_layers;
                tile.progression_order = cod.progression_order;
                for component in &mut tile.component_infos {
                    component.coding_style.flags.raw |= cod.component_parameters.flags.raw;
                    component.coding_style.parameters = cod.component_parameters.clone().parameters;
                }
                coding_style_changed = true;
            }
            markers::COC => {
                let (component_index, coc) = codestream::coc_marker(reader)?;
                let old = tile
                    .component_infos
                    .get_mut(usize::from(component_index))
                    .ok_or(DecodeError::Malformed)?;
                old.coding_style.parameters = coc.parameters;
                old.coding_style.flags.raw |= coc.flags.raw;
                coding_style_changed = true;
            }
            markers::QCD => {
                let qcd = codestream::qcd_marker(reader).ok_or(DecodeError::Malformed)?;
                for component_info in &mut tile.component_infos {
                    component_info.quantization_info = qcd.clone();
                }
            }
            markers::QCC => {
                let (component_index, qcc) =
                    codestream::qcc_marker(reader).ok_or(DecodeError::Malformed)?;
                tile.component_infos
                    .get_mut(usize::from(component_index))
                    .ok_or(DecodeError::Malformed)?
                    .quantization_info = qcc;
            }
            markers::PPT => {
                if !main_header.ppm_packets.is_empty() {
                    bail!(DecodeError::Malformed);
                }
                ppt_headers.push(ppt_marker(reader).ok_or(DecodeError::Malformed)?);
            }
            markers::PLT => {
                plt_markers.push(plt_marker(reader).ok_or(DecodeError::Malformed)?);
            }
            markers::RGN => {
                let (component_index, roi_shift) =
                    codestream::rgn_marker(reader).ok_or(DecodeError::Malformed)?;
                tile.component_infos
                    .get_mut(usize::from(component_index))
                    .ok_or(DecodeError::Malformed)?
                    .roi_shift = roi_shift;
            }
            markers::POC => {
                tile.progression_changes
                    .extend(codestream::poc_marker(reader).ok_or(DecodeError::Malformed)?);
            }
            markers::COM => {
                skip_marker_segment(reader).ok_or(DecodeError::Malformed)?;
            }
            _ => {
                bail!(DecodeError::Unsupported);
            }
        }

        marker_budget.record(reader.offset() - seg_start)?;
    }

    if coding_style_changed {
        validate_resource_limits(
            &main_header.size_data,
            &tile.component_infos,
            tile.num_layers,
        )?;
    }

    if !saw_sod {
        return Ok(true);
    }

    let sod_end = reader.offset();
    let tail_len = reader.tail().map_or(0, |t| t.len());
    let body_bytes = if let Some(end) = declared_end {
        let Some(remaining) = end.checked_sub(sod_end) else {
            return Ok(true);
        };
        let avail = remaining.min(tail_len);
        reader.read_bytes(avail).unwrap_or(&[])
    } else {
        let data = reader.tail().unwrap_or(&[]);
        reader.jump_to_end();
        data
    };

    ppt_headers.sort_by_key(|p| p.sequence_idx);
    let mut headers: Vec<_> = ppt_headers.iter().map(|i| BitReader::new(i.data)).collect();
    if let Some(ppm_marker) = main_header.ppm_packets.get(tile_part_idx) {
        headers.push(BitReader::new(ppm_marker));
    }

    let part = if !headers.is_empty() {
        TilePart::Separated(SeparatedTilePart {
            headers,
            active_header_reader: 0,
            body: BitReader::new(body_bytes),
            body_offset: sod_end,
        })
    } else {
        TilePart::Merged(MergedTilePart {
            data: BitReader::new(body_bytes),
            body_offset: sod_end,
        })
    };

    parts.push(InspectTilePart {
        part,
        sod_end,
        declared_end,
        plt_markers,
    });

    Ok(true)
}

fn plt_marker<'a>(reader: &mut BitReader<'a>) -> Option<PltMarkerData<'a>> {
    let length = reader.read_u16()?.checked_sub(2)?;
    let payload_len = length.checked_sub(1)?;
    let sequence_idx = reader.read_byte()?;
    Some(PltMarkerData {
        sequence_idx,
        data: reader.read_bytes(usize::from(payload_len))?,
    })
}

fn parse_plt_lengths(markers: &[PltMarkerData<'_>]) -> Result<Vec<usize>> {
    let mut lengths = Vec::new();
    for marker in markers {
        let mut i = 0_usize;
        while i < marker.data.len() {
            let mut value = 0_u64;
            let mut count = 0_u8;
            loop {
                let byte = *marker.data.get(i).ok_or(DecodeError::Malformed)?;
                i += 1;
                count += 1;
                if count > 5 {
                    bail!(DecodeError::Malformed);
                }
                value = (value << 7) | u64::from(byte & 0x7F);
                if (byte & 0x80) == 0 {
                    break;
                }
            }
            lengths.push(usize::try_from(value).map_err(|_| DecodeError::TooLarge)?);
        }
    }
    Ok(lengths)
}

fn count_expected(
    tile: &Tile<'_>,
    main_header: &Header,
    levels: u8,
    stride: usize,
    buckets: &mut [Bucket],
) -> Result<()> {
    let has_poc =
        !tile.progression_changes.is_empty() || !main_header.progression_changes.is_empty();
    if has_poc {
        let prog = progression_iterator(tile, main_header)?;
        for data in prog {
            let Some(info) = tile.component_infos.get(usize::from(data.component)) else {
                continue;
            };
            let max_d = info
                .num_decomposition_levels()
                .saturating_sub(data.resolution)
                .min(levels);
            if let Some(bucket) =
                buckets.get_mut(usize::from(max_d) * stride + usize::from(data.layer_num))
            {
                bucket.expected = bucket.expected.saturating_add(1);
            }
        }
        return Ok(());
    }

    for info in &tile.component_infos {
        let ct = ComponentTile::new(tile, info);
        let n_l = info.num_decomposition_levels();
        for r in 0..info.num_resolution_levels() {
            let num_precincts = ResolutionTile::new(ct, r).num_precincts();
            let max_d = n_l.saturating_sub(r).min(levels);
            for l in 0..tile.num_layers {
                if let Some(bucket) = buckets.get_mut(usize::from(max_d) * stride + usize::from(l))
                {
                    bucket.expected = bucket.expected.saturating_add(num_precincts);
                }
            }
        }
    }

    Ok(())
}

fn record_packet(
    component_infos: &[ComponentInfo],
    prog_data: ProgressionData,
    pkt_end: usize,
    present: bool,
    levels: u8,
    stride: usize,
    buckets: &mut [Bucket],
) {
    let Some(info) = component_infos.get(usize::from(prog_data.component)) else {
        return;
    };
    let max_d = info
        .num_decomposition_levels()
        .saturating_sub(prog_data.resolution)
        .min(levels);
    if let Some(bucket) =
        buckets.get_mut(usize::from(max_d) * stride + usize::from(prog_data.layer_num))
    {
        bucket.known = bucket.known.saturating_add(1);
        bucket.max_end = bucket.max_end.max(pkt_end);
        if present {
            bucket.present = bucket.present.saturating_add(1);
        }
    }
}

fn build_header_state(tile: &Tile<'_>, state: &mut HeaderState, cancel: Cancel<'_>) -> Result<()> {
    state.code_blocks.clear();
    state.precincts.clear();
    state.tag_tree_nodes.clear();
    state.sub_bands.clear();
    state.decompositions.clear();
    state.components.clear();

    for component_tile in tile.component_tiles() {
        let d_start = state.decompositions.len();
        let mut resolution_tiles = component_tile.resolution_tiles();

        let build_sub_band = |sub_band_type: SubBandType,
                              resolution_tile: &ResolutionTile<'_>,
                              state: &mut HeaderState|
         -> Result<usize> {
            let sub_band_rect = resolution_tile.sub_band_rect(sub_band_type);
            let p_start = state.precincts.len();

            for precinct_data in resolution_tile.precincts().ok_or(DecodeError::Malformed)? {
                cancel.check()?;
                let precinct_rect = precinct_data.rect;
                let cb_width = resolution_tile.code_block_width();
                let cb_height = resolution_tile.code_block_height();

                let cb_x0 = (u32::max(precinct_rect.x0, sub_band_rect.x0) / cb_width) * cb_width;
                let cb_y0 = (u32::max(precinct_rect.y0, sub_band_rect.y0) / cb_height) * cb_height;
                let cb_x1 =
                    (u32::min(precinct_rect.x1, sub_band_rect.x1).div_ceil(cb_width)) * cb_width;
                let cb_y1 =
                    (u32::min(precinct_rect.y1, sub_band_rect.y1).div_ceil(cb_height)) * cb_height;
                let code_block_area = IntRect::from_ltrb(cb_x0, cb_y0, cb_x1, cb_y1);

                let code_blocks_x = if sub_band_rect.width() == 0 {
                    0
                } else {
                    code_block_area.width() / cb_width
                };
                let code_blocks_y = if sub_band_rect.height() == 0 {
                    0
                } else {
                    code_block_area.height() / cb_height
                };

                let cb_start = state.code_blocks.len();
                for y_idx in 0..code_blocks_y {
                    cancel.check()?;
                    for x_idx in 0..code_blocks_x {
                        state.code_blocks.push(InspectCodeBlock {
                            x_idx,
                            y_idx,
                            has_been_included: false,
                            l_block: 3,
                            number_of_coding_passes: 0,
                            non_empty_layer_count: 0,
                        });
                    }
                }
                let cb_end = state.code_blocks.len();

                let code_inclusion_tree =
                    TagTree::new(code_blocks_x, code_blocks_y, &mut state.tag_tree_nodes);
                let zero_bitplane_tree =
                    TagTree::new(code_blocks_x, code_blocks_y, &mut state.tag_tree_nodes);

                state.precincts.push(InspectPrecinct {
                    code_blocks: cb_start..cb_end,
                    code_inclusion_tree,
                    zero_bitplane_tree,
                });
            }

            let p_end = state.precincts.len();
            let sb_idx = state.sub_bands.len();
            state.sub_bands.push(p_start..p_end);
            Ok(sb_idx)
        };

        let ll_tile = resolution_tiles.next().ok_or(DecodeError::Malformed)?;
        let first_ll = build_sub_band(SubBandType::LowLow, &ll_tile, state)?;

        for res_tile in resolution_tiles {
            let sub_bands = [
                build_sub_band(SubBandType::HighLow, &res_tile, state)?,
                build_sub_band(SubBandType::LowHigh, &res_tile, state)?,
                build_sub_band(SubBandType::HighHigh, &res_tile, state)?,
            ];
            state.decompositions.push(sub_bands);
        }

        let d_end = state.decompositions.len();
        state.components.push(ComponentSubBands {
            first_ll,
            decompositions: d_start..d_end,
        });
    }

    Ok(())
}

fn parse_tile_part_packets(
    tp: &mut InspectTilePart<'_>,
    prog: &mut dyn Iterator<Item = ProgressionData>,
    component_infos: &[ComponentInfo],
    state: &mut HeaderState,
    data_len: usize,
    levels: u8,
    stride: usize,
    buckets: &mut [Bucket],
    cancel: Cancel<'_>,
) -> Result<()> {
    while !tp.part.header().at_end() {
        cancel.check()?;
        let Some((prog_data, body_len)) =
            parse_packet_header(&mut tp.part, prog, component_infos, state)
        else {
            break;
        };

        let Some(pkt_end) = tp
            .sod_end
            .checked_add(tp.part.body().offset())
            .and_then(|o| o.checked_add(body_len))
        else {
            bail!(DecodeError::Malformed);
        };
        let present = pkt_end <= data_len && tp.declared_end.is_none_or(|end| pkt_end <= end);
        record_packet(
            component_infos,
            prog_data,
            pkt_end,
            present,
            levels,
            stride,
            buckets,
        );

        if tp.part.body().skip_bytes(body_len).is_none() {
            break;
        }
    }

    Ok(())
}

fn parse_packet_header(
    tile_part: &mut TilePart<'_>,
    prog: &mut dyn Iterator<Item = ProgressionData>,
    component_infos: &[ComponentInfo],
    state: &mut HeaderState,
) -> Option<(ProgressionData, usize)> {
    let prog_data = prog.next()?;
    let resolution = prog_data.resolution;
    let component_info = component_infos.get(usize::from(prog_data.component))?;
    let comp = state.components.get(usize::from(prog_data.component))?;

    let (sb_count, sb_indices) = if resolution == 0 {
        (1, [comp.first_ll, 0, 0])
    } else {
        let indices = *state
            .decompositions
            .get(comp.decompositions.clone())?
            .get(usize::from(resolution) - 1)?;
        (3, indices)
    };

    let body_reader = tile_part.body();
    if component_info.coding_style.flags.may_use_sop_markers()
        && body_reader.peek_marker() == Some(markers::SOP)
    {
        body_reader.read_marker().ok()?;
        body_reader.skip_bytes(4)?;
    }

    let header_reader = tile_part.header();
    let zero_length = header_reader.read_bits_with_stuffing(1).ok()? == 0;
    let mut body_len = 0_usize;

    if !zero_length {
        for &sb_idx in sb_indices.get(..sb_count)? {
            let sb_len =
                resolve_sub_band_length(sb_idx, &prog_data, header_reader, state, component_info)?;
            body_len = body_len.checked_add(sb_len)?;
        }
    }

    header_reader.align();

    if component_info.coding_style.flags.uses_eph_marker()
        && header_reader.read_marker().ok()? != markers::EPH
    {
        return None;
    }

    Some((prog_data, body_len))
}

fn resolve_sub_band_length(
    sub_band_idx: usize,
    prog_data: &ProgressionData,
    reader: &mut BitReader<'_>,
    state: &mut HeaderState,
    component_info: &ComponentInfo,
) -> Option<usize> {
    const MAX_CODING_PASSES: u8 = 1 + 3 * (MAX_BITPLANE_COUNT - 1);

    let precincts_range = state.sub_bands.get(sub_band_idx)?.clone();
    let precinct = state
        .precincts
        .get_mut(precincts_range)?
        .get_mut(usize::try_from(prog_data.precinct).ok()?)?;
    let code_blocks = state.code_blocks.get_mut(precinct.code_blocks.clone())?;

    let mut total_len = 0_usize;
    let style = component_info.code_block_style();

    for cb in code_blocks {
        let is_included = if cb.has_been_included {
            reader.read_bits_with_stuffing(1).ok()? == 1
        } else {
            precinct
                .code_inclusion_tree
                .read(
                    cb.x_idx,
                    cb.y_idx,
                    reader,
                    u32::from(prog_data.layer_num) + 1,
                    &mut state.tag_tree_nodes,
                )
                .ok()?
                <= u32::from(prog_data.layer_num)
        };

        if !is_included {
            continue;
        }

        if !cb.has_been_included {
            precinct
                .zero_bitplane_tree
                .read(
                    cb.x_idx,
                    cb.y_idx,
                    reader,
                    u32::MAX,
                    &mut state.tag_tree_nodes,
                )
                .ok()?;
            cb.has_been_included = true;
        }

        let added_passes = if reader.peak_bits_with_stuffing(9) == Some(0x1ff) {
            reader.read_bits_with_stuffing(9).ok()?;
            reader.read_bits_with_stuffing(7).ok()? + 37
        } else if reader.peak_bits_with_stuffing(4) == Some(0x0f) {
            reader.read_bits_with_stuffing(4).ok()?;
            reader.read_bits_with_stuffing(5).ok()? + 6
        } else if reader.peak_bits_with_stuffing(4) == Some(0b1110) {
            reader.read_bits_with_stuffing(4).ok()?;
            5
        } else if reader.peak_bits_with_stuffing(4) == Some(0b1101) {
            reader.read_bits_with_stuffing(4).ok()?;
            4
        } else if reader.peak_bits_with_stuffing(4) == Some(0b1100) {
            reader.read_bits_with_stuffing(4).ok()?;
            3
        } else if reader.peak_bits_with_stuffing(2) == Some(0b10) {
            reader.read_bits_with_stuffing(2).ok()?;
            2
        } else if reader.peak_bits_with_stuffing(1) == Some(0) {
            reader.read_bits_with_stuffing(1).ok()?;
            1
        } else {
            return None;
        } as u8;

        let mut k = 0_u32;
        while reader.read_bits_with_stuffing(1).ok()? == 1 {
            k = k.checked_add(1)?;
        }
        cb.l_block = cb.l_block.checked_add(k)?;

        let prev_passes = cb.number_of_coding_passes;
        let cumulative_passes = prev_passes.checked_add(added_passes)?;
        if cumulative_passes > MAX_CODING_PASSES {
            return None;
        }

        let get_segment_idx = |pass_idx: u8| {
            if style.termination_on_each_pass {
                pass_idx
            } else if style.selective_arithmetic_coding_bypass {
                segment_idx_for_bypass(pass_idx)
            } else {
                cb.non_empty_layer_count
            }
        };

        let mut last_segment = get_segment_idx(prev_passes);
        let mut passes_for_segment = 0_u8;

        for coding_pass in prev_passes..cumulative_passes {
            let segment = get_segment_idx(coding_pass);
            if segment != last_segment {
                let length_bits = cb.l_block + passes_for_segment.ilog2();
                let seg_len = reader
                    .read_bits_with_stuffing(u8::try_from(length_bits).ok()?)
                    .ok()?;
                total_len = total_len.checked_add(seg_len as usize)?;
                last_segment = segment;
                passes_for_segment = 1;
            } else {
                passes_for_segment += 1;
            }
        }

        if passes_for_segment > 0 {
            let length_bits = cb.l_block + passes_for_segment.ilog2();
            let seg_len = reader
                .read_bits_with_stuffing(u8::try_from(length_bits).ok()?)
                .ok()?;
            total_len = total_len.checked_add(seg_len as usize)?;
        }

        cb.number_of_coding_passes = cumulative_passes;
        cb.non_empty_layer_count += 1;
    }

    Some(total_len)
}
