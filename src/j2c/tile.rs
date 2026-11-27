use core::ops::Range;
use std::vec;
use std::vec::Vec;

use super::build::{PrecinctData, SubBandType};
use super::codestream::{
    ComponentInfo, Header, MarkerBudget, ProgressionChange, ProgressionOrder, markers,
    skip_marker_segment, validate_resource_limits,
};
use super::rect::IntRect;
use crate::error::{DecodeError, Result, bail};
use crate::j2c::codestream;
use crate::reader::BitReader;

pub(crate) const MAX_TILE_PARTS_PER_TILE: usize = 1024;

#[derive(Clone, Debug)]
pub(crate) struct TilePartData {
    pub(crate) body: Range<usize>,
    pub(crate) ppt_headers: Vec<Range<usize>>,
    pub(crate) ppm_idx: Option<usize>,
}

impl TilePartData {
    pub(crate) fn to_tile_part<'a>(
        &self,
        codestream: &'a [u8],
        header: &'a Header,
    ) -> Option<TilePart<'a>> {
        let body_bytes = codestream.get(self.body.clone())?;
        if self.ppt_headers.is_empty() && self.ppm_idx.is_none() {
            return Some(TilePart::Merged(MergedTilePart {
                data: BitReader::new(body_bytes),
                body_offset: self.body.start,
            }));
        }

        let mut headers =
            Vec::with_capacity(self.ppt_headers.len() + usize::from(self.ppm_idx.is_some()));
        for range in &self.ppt_headers {
            headers.push(BitReader::new(codestream.get(range.clone())?));
        }
        if let Some(ppm_idx) = self.ppm_idx
            && let Some(ppm_marker) = header.ppm_packets.get(ppm_idx)
        {
            headers.push(BitReader::new(ppm_marker));
        }

        Some(TilePart::Separated(SeparatedTilePart {
            headers,
            active_header_reader: 0,
            body: BitReader::new(body_bytes),
            body_offset: self.body.start,
        }))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Tile {
    pub(crate) tile_parts: Vec<TilePartData>,
    pub(crate) component_infos: Option<Vec<ComponentInfo>>,
    pub(crate) rect: IntRect,
    pub(crate) progression_order: ProgressionOrder,
    pub(crate) progression_changes: Vec<ProgressionChange>,
    pub(crate) num_layers: u8,
    pub(crate) mct: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct MergedTilePart<'a> {
    pub(crate) data: BitReader<'a>,
    pub(crate) body_offset: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct SeparatedTilePart<'a> {
    pub(crate) headers: Vec<BitReader<'a>>,
    pub(crate) active_header_reader: usize,
    pub(crate) body: BitReader<'a>,
    pub(crate) body_offset: usize,
}

#[derive(Clone, Debug)]
pub(crate) enum TilePart<'a> {
    Merged(MergedTilePart<'a>),
    Separated(SeparatedTilePart<'a>),
}

impl<'a> TilePart<'a> {
    pub(crate) fn header(&mut self) -> &mut BitReader<'a> {
        match self {
            TilePart::Merged(m) => &mut m.data,
            TilePart::Separated(s) => {
                if let Some(active) = s.headers.get(s.active_header_reader)
                    && active.at_end()
                    && s.active_header_reader + 1 < s.headers.len()
                {
                    s.active_header_reader += 1;
                }

                if let Some(reader) = s.headers.get_mut(s.active_header_reader) {
                    reader
                } else {
                    &mut s.body
                }
            }
        }
    }

    pub(crate) fn body(&mut self) -> &mut BitReader<'a> {
        match self {
            TilePart::Merged(m) => &mut m.data,
            TilePart::Separated(s) => &mut s.body,
        }
    }

    pub(crate) fn body_offset(&self) -> usize {
        match self {
            TilePart::Merged(m) => m.body_offset,
            TilePart::Separated(s) => s.body_offset,
        }
    }
}

impl Tile {
    pub(crate) fn new(idx: u32, header: &Header) -> Self {
        let mut tile = Self {
            tile_parts: vec![],
            component_infos: None,
            rect: IntRect::from_ltrb(0, 0, 0, 0),
            progression_order: header.global_coding_style.progression_order,
            progression_changes: vec![],
            mct: header.global_coding_style.mct,
            num_layers: header.global_coding_style.num_layers,
        };
        tile.reset(idx, header);
        tile
    }

    pub(crate) fn reset(&mut self, idx: u32, header: &Header) {
        let size_data = &header.size_data;

        let x_coord = u64::from(size_data.tile_x_coord(idx));
        let y_coord = u64::from(size_data.tile_y_coord(idx));

        let x0 = (x_coord * u64::from(size_data.tile_width)) as u32;
        let y0 = (y_coord * u64::from(size_data.tile_height)) as u32;

        let x1 = u64::min(
            (x_coord + 1) * u64::from(size_data.tile_width),
            u64::from(size_data.reference_grid_width),
        ) as u32;
        let y1 = u64::min(
            (y_coord + 1) * u64::from(size_data.tile_height),
            u64::from(size_data.reference_grid_height),
        ) as u32;

        self.tile_parts.clear();
        self.component_infos = None;
        self.rect = IntRect::from_ltrb(x0, y0, x1, y1);
        self.progression_order = header.global_coding_style.progression_order;
        self.progression_changes.clear();
        self.mct = header.global_coding_style.mct;
        self.num_layers = header.global_coding_style.num_layers;
    }

    pub(crate) fn component_infos<'a>(&'a self, header: &'a Header) -> &'a [ComponentInfo] {
        self.component_infos
            .as_deref()
            .unwrap_or(&header.component_infos)
    }

    pub(crate) fn component_infos_mut(&mut self, header: &Header) -> &mut Vec<ComponentInfo> {
        self.component_infos
            .get_or_insert_with(|| header.component_infos.clone())
    }

    pub(crate) fn component_tiles<'a>(
        &'a self,
        header: &'a Header,
    ) -> impl Iterator<Item = ComponentTile<'a>> {
        self.component_infos(header)
            .iter()
            .map(|i| ComponentTile::new(self, i))
    }
}

pub(crate) fn parse(
    reader: &mut BitReader<'_>,
    main_header: &Header,
    tiles_buf: &mut Vec<Tile>,
) -> Result<usize> {
    let tile_count = main_header.size_data.num_tiles()? as usize;
    for (idx, tile) in tiles_buf.iter_mut().take(tile_count).enumerate() {
        tile.reset(idx as u32, main_header);
    }
    while tiles_buf.len() < tile_count {
        let idx = tiles_buf.len() as u32;
        tiles_buf.push(Tile::new(idx, main_header));
    }
    let tiles = tiles_buf
        .get_mut(..tile_count)
        .ok_or(DecodeError::Malformed)?;

    let mut marker_budget = main_header.marker_budget;
    let mut tile_part_idx = 0;

    while reader.peek_marker() == Some(markers::SOT) {
        if !parse_tile_part(
            reader,
            main_header,
            tiles,
            tile_part_idx,
            &mut marker_budget,
        )? {
            return Ok(tile_count);
        }
        tile_part_idx += 1;
    }

    if main_header.strict {
        let tail = reader.tail().unwrap_or(&[]);
        if !tail.is_empty() && tail != [0xFF] && reader.read_marker()? != markers::EOC {
            bail!(DecodeError::Malformed);
        }
    }

    Ok(tile_count)
}

fn parse_tile_part(
    reader: &mut BitReader<'_>,
    main_header: &Header,
    tiles: &mut [Tile],
    tile_part_idx: usize,
    marker_budget: &mut MarkerBudget,
) -> Result<bool> {
    if reader.peek_bytes(12).is_none() {
        return Ok(false);
    }

    if reader.read_marker()? != markers::SOT {
        bail!(DecodeError::Malformed);
    }

    let sot_start = reader.offset();
    let tile_part_header = sot_marker(reader).ok_or(DecodeError::Malformed)?;
    marker_budget.record(reader.offset() - sot_start)?;

    if tile_part_header.tile_index as u32 >= main_header.size_data.num_tiles()? {
        bail!(DecodeError::Malformed);
    }

    let declared_len = if tile_part_header.tile_part_length == 0 {
        None
    } else {
        Some(
            (tile_part_header.tile_part_length as usize)
                .checked_sub(12)
                .ok_or(DecodeError::Malformed)?,
        )
    };

    let start = reader.offset();

    let tile = tiles
        .get_mut(tile_part_header.tile_index as usize)
        .ok_or(DecodeError::Malformed)?;
    if tile.tile_parts.len() >= MAX_TILE_PARTS_PER_TILE {
        bail!(DecodeError::TooLarge);
    }

    let mut ppt_headers: Vec<(u8, Range<usize>)> = vec![];
    let mut coding_style_changed = false;
    let mut saw_sod = false;

    loop {
        if let Some(len) = declared_len
            && reader.offset() - start >= len
        {
            if coding_style_changed {
                validate_resource_limits(
                    &main_header.size_data,
                    tile.component_infos(main_header),
                    tile.num_layers,
                )?;
            }
            if reader.at_end() {
                return Ok(false);
            }
            return if main_header.strict {
                Err(DecodeError::Malformed)
            } else {
                Ok(true)
            };
        }

        let tail = reader.tail().unwrap_or(&[]);
        if tail.is_empty() || tail == [0xFF] {
            if coding_style_changed {
                validate_resource_limits(
                    &main_header.size_data,
                    tile.component_infos(main_header),
                    tile.num_layers,
                )?;
            }
            return Ok(false);
        }

        let Some(marker) = reader.peek_marker() else {
            if coding_style_changed {
                validate_resource_limits(
                    &main_header.size_data,
                    tile.component_infos(main_header),
                    tile.num_layers,
                )?;
            }
            return if main_header.strict {
                Err(DecodeError::Malformed)
            } else {
                Ok(false)
            };
        };

        if marker == markers::EOC {
            if main_header.strict {
                bail!(DecodeError::Malformed);
            }
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
            if coding_style_changed {
                validate_resource_limits(
                    &main_header.size_data,
                    tile.component_infos(main_header),
                    tile.num_layers,
                )?;
            }
            return Ok(false);
        };
        let seg_len = u16::from_be_bytes([hi, lo]);
        if seg_len < 2 {
            bail!(DecodeError::Malformed);
        }
        if reader.peek_bytes(2 + usize::from(seg_len)).is_none() {
            if coding_style_changed {
                validate_resource_limits(
                    &main_header.size_data,
                    tile.component_infos(main_header),
                    tile.num_layers,
                )?;
            }
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

                for component in tile.component_infos_mut(main_header) {
                    component.coding_style.flags.raw |= cod.component_parameters.flags.raw;
                    component.coding_style.parameters = cod.component_parameters.clone().parameters;
                }
                coding_style_changed = true;
            }
            markers::COC => {
                let (component_index, coc) = codestream::coc_marker(reader)?;

                let old = tile
                    .component_infos_mut(main_header)
                    .get_mut(component_index as usize)
                    .ok_or(DecodeError::Malformed)?;

                old.coding_style.parameters = coc.parameters;
                old.coding_style.flags.raw |= coc.flags.raw;
                coding_style_changed = true;
            }
            markers::QCD => {
                let qcd = codestream::qcd_marker(reader).ok_or(DecodeError::Malformed)?;

                for component_info in tile.component_infos_mut(main_header) {
                    component_info.quantization_info = qcd.clone();
                }
            }
            markers::QCC => {
                let (component_index, qcc) =
                    codestream::qcc_marker(reader).ok_or(DecodeError::Malformed)?;

                tile.component_infos_mut(main_header)
                    .get_mut(component_index as usize)
                    .ok_or(DecodeError::Malformed)?
                    .quantization_info = qcc;
            }
            markers::PPT => {
                if !main_header.ppm_packets.is_empty() {
                    bail!(DecodeError::Malformed);
                }

                let length = reader
                    .read_u16()
                    .and_then(|l| l.checked_sub(2))
                    .ok_or(DecodeError::Malformed)?;
                let header_len = length.checked_sub(1).ok_or(DecodeError::Malformed)?;
                let sequence_idx = reader.read_byte().ok_or(DecodeError::Malformed)?;
                let data_start = reader.offset();
                let bytes = reader
                    .read_bytes(header_len as usize)
                    .ok_or(DecodeError::Malformed)?;
                ppt_headers.push((sequence_idx, data_start..data_start + bytes.len()));
            }
            markers::RGN => {
                let (component_index, roi_shift) =
                    codestream::rgn_marker(reader).ok_or(DecodeError::Malformed)?;

                tile.component_infos_mut(main_header)
                    .get_mut(component_index as usize)
                    .ok_or(DecodeError::Malformed)?
                    .roi_shift = roi_shift;
            }
            markers::POC => {
                tile.progression_changes
                    .extend(codestream::poc_marker(reader).ok_or(DecodeError::Malformed)?);
            }
            markers::PLT => {
                skip_marker_segment(reader).ok_or(DecodeError::Malformed)?;
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
            tile.component_infos(main_header),
            tile.num_layers,
        )?;
    }

    if !saw_sod {
        return Ok(true);
    }

    let body_offset = reader.offset();
    let data = if let Some(len) = declared_len {
        let Some(remaining_bytes) = len.checked_sub(reader.offset() - start) else {
            return if main_header.strict {
                Err(DecodeError::Malformed)
            } else {
                Ok(true)
            };
        };
        let avail = remaining_bytes.min(reader.tail().map_or(0, |t| t.len()));
        reader.read_bytes(avail).ok_or(DecodeError::Malformed)?
    } else {
        let data = reader.tail().ok_or(DecodeError::Malformed)?;
        reader.jump_to_end();
        data
    };

    ppt_headers.sort_by_key(|(seq, _)| *seq);
    let ppt_ranges = if ppt_headers.is_empty() {
        vec![]
    } else {
        ppt_headers.into_iter().map(|(_, r)| r).collect()
    };
    let ppm_idx = (tile_part_idx < main_header.ppm_packets.len()).then_some(tile_part_idx);

    tile.tile_parts.push(TilePartData {
        body: body_offset..body_offset + data.len(),
        ppt_headers: ppt_ranges,
        ppm_idx,
    });

    Ok(true)
}

#[derive(Debug, Copy, Clone)]
pub(crate) struct ComponentTile<'a> {
    pub(crate) tile: &'a Tile,
    pub(crate) component_info: &'a ComponentInfo,
    pub(crate) rect: IntRect,
}

impl<'a> ComponentTile<'a> {
    pub(crate) fn new(tile: &'a Tile, component_info: &'a ComponentInfo) -> Self {
        ComponentTile {
            tile,
            component_info,
            rect: tile.rect,
        }
    }

    pub(crate) fn resolution_tiles(&self) -> impl Iterator<Item = ResolutionTile<'_>> {
        (0..self
            .component_info
            .coding_style
            .parameters
            .num_resolution_levels)
            .map(|r| ResolutionTile::new(*self, r))
    }
}

pub(crate) struct ResolutionTile<'a> {
    pub(crate) resolution: u8,
    pub(crate) decomposition_level: u8,
    pub(crate) component_tile: ComponentTile<'a>,
    pub(crate) rect: IntRect,
}

impl<'a> ResolutionTile<'a> {
    pub(crate) fn new(component_tile: ComponentTile<'a>, resolution: u8) -> Self {
        assert!(
            component_tile
                .component_info
                .coding_style
                .parameters
                .num_resolution_levels
                > resolution
        );

        let rect = {
            let n_l = component_tile
                .component_info
                .coding_style
                .parameters
                .num_decomposition_levels;

            let tx0 = (component_tile.rect.x0 as u64)
                .div_ceil(2_u64.pow(n_l as u32 - resolution as u32)) as u32;
            let ty0 = (component_tile.rect.y0 as u64)
                .div_ceil(2_u64.pow(n_l as u32 - resolution as u32)) as u32;
            let tx1 = (component_tile.rect.x1 as u64)
                .div_ceil(2_u64.pow(n_l as u32 - resolution as u32)) as u32;
            let ty1 = (component_tile.rect.y1 as u64)
                .div_ceil(2_u64.pow(n_l as u32 - resolution as u32)) as u32;

            IntRect::from_ltrb(tx0, ty0, tx1, ty1)
        };

        let decomposition_level = {
            if resolution == 0 {
                component_tile
                    .component_info
                    .coding_style
                    .parameters
                    .num_decomposition_levels
            } else {
                component_tile
                    .component_info
                    .coding_style
                    .parameters
                    .num_decomposition_levels
                    - (resolution - 1)
            }
        };

        ResolutionTile {
            resolution,
            decomposition_level,
            component_tile,
            rect,
        }
    }

    pub(crate) fn sub_band_rect(&self, sub_band_type: SubBandType) -> IntRect {
        if self.resolution == 0 {
            assert_eq!(sub_band_type, SubBandType::LowLow);
        }

        let xo_b = if matches!(sub_band_type, SubBandType::HighLow | SubBandType::HighHigh) {
            1
        } else {
            0
        };
        let yo_b = if matches!(sub_band_type, SubBandType::LowHigh | SubBandType::HighHigh) {
            1
        } else {
            0
        };

        let mut numerator_x = 0;
        let mut numerator_y = 0;

        if self.decomposition_level > 0 {
            numerator_x = 2_u64.pow(self.decomposition_level as u32 - 1) * xo_b as u64;
            numerator_y = 2_u64.pow(self.decomposition_level as u32 - 1) * yo_b as u64;
        }

        let denominator = 2_u64.pow(self.decomposition_level as u32);

        let tbx_0 = (self.component_tile.rect.x0 as u64)
            .saturating_sub(numerator_x)
            .div_ceil(denominator) as u32;
        let tbx_1 = (self.component_tile.rect.x1 as u64)
            .saturating_sub(numerator_x)
            .div_ceil(denominator) as u32;
        let tby_0 = (self.component_tile.rect.y0 as u64)
            .saturating_sub(numerator_y)
            .div_ceil(denominator) as u32;
        let tby_1 = (self.component_tile.rect.y1 as u64)
            .saturating_sub(numerator_y)
            .div_ceil(denominator) as u32;

        IntRect::from_ltrb(tbx_0, tby_0, tbx_1, tby_1)
    }

    fn precinct_exponent_x(&self) -> u8 {
        self.component_tile
            .component_info
            .coding_style
            .parameters
            .precinct_exponents
            .get(self.resolution as usize)
            .map_or(15, |p| p.0)
    }

    fn precinct_exponent_y(&self) -> u8 {
        self.component_tile
            .component_info
            .coding_style
            .parameters
            .precinct_exponents
            .get(self.resolution as usize)
            .map_or(15, |p| p.1)
    }

    fn num_precincts_x(&self) -> u32 {
        let IntRect { x0, x1, .. } = self.rect;

        if x0 == x1 {
            0
        } else {
            x1.div_ceil(2_u32.pow(self.precinct_exponent_x() as u32))
                - x0 / 2_u32.pow(self.precinct_exponent_x() as u32)
        }
    }

    fn num_precincts_y(&self) -> u32 {
        let IntRect { y0, y1, .. } = self.rect;

        if y0 == y1 {
            0
        } else {
            y1.div_ceil(2_u32.pow(self.precinct_exponent_y() as u32))
                - y0 / 2_u32.pow(self.precinct_exponent_y() as u32)
        }
    }

    pub(crate) fn num_precincts(&self) -> u64 {
        self.num_precincts_x() as u64 * self.num_precincts_y() as u64
    }

    pub(crate) fn precincts(&self) -> Option<impl Iterator<Item = PrecinctData>> {
        let num_precincts_y = self.num_precincts_y();
        let num_precincts_x = self.num_precincts_x();

        let mut ppx = self.precinct_exponent_x();
        let mut ppy = self.precinct_exponent_y();

        let mut y_start = (self.rect.y0 / (1 << ppy)) * (1 << ppy);
        let mut x_start = (self.rect.x0 / (1 << ppx)) * (1 << ppx);

        if self.resolution > 0 {
            ppx = ppx.checked_sub(1)?;
            ppy = ppy.checked_sub(1)?;

            x_start /= 2;
            y_start /= 2;
        }

        let ppx_pow2 = 1_u32 << ppx;
        let ppy_pow2 = 1_u32 << ppy;

        let nl_minus_r = self
            .component_tile
            .component_info
            .num_decomposition_levels()
            - self.resolution;

        let x_stride =
            1_u32.checked_shl(self.precinct_exponent_x().checked_add(nl_minus_r)? as u32)?;
        let y_stride =
            1_u32.checked_shl(self.precinct_exponent_y().checked_add(nl_minus_r)? as u32)?;

        let mut r_x = self.component_tile.tile.rect.x0;
        let mut r_y = self.component_tile.tile.rect.y0;

        if !r_x.is_multiple_of(x_stride)
            && (self.rect.x0 * (1 << nl_minus_r)).is_multiple_of(x_stride)
        {
            r_x = r_x.checked_next_multiple_of(x_stride)?;
        }

        if !r_y.is_multiple_of(y_stride)
            && (self.rect.y0 * (1 << nl_minus_r)).is_multiple_of(y_stride)
        {
            r_y = r_y.checked_next_multiple_of(y_stride)?;
        }

        let iter = (0..num_precincts_y).flat_map(move |y| {
            let y0 = y * ppy_pow2 + y_start;
            let mut r_x = r_x;

            let res = (0..num_precincts_x).map(move |x| {
                let x0 = x * ppx_pow2 + x_start;

                let data = PrecinctData {
                    r_x,
                    r_y,
                    rect: IntRect::from_xywh(x0, y0, ppx_pow2, ppy_pow2),
                    idx: num_precincts_x as u64 * y as u64 + x as u64,
                };

                r_x = (r_x + 1).next_multiple_of(x_stride);

                data
            });

            r_y = (r_y + 1).next_multiple_of(y_stride);

            res
        });

        Some(iter)
    }

    pub(crate) fn code_block_width(&self) -> u32 {
        let xcb = self
            .component_tile
            .component_info
            .coding_style
            .parameters
            .code_block_width;

        let xcb = if self.resolution > 0 {
            u8::min(xcb, self.precinct_exponent_x() - 1)
        } else {
            u8::min(xcb, self.precinct_exponent_x())
        };

        2_u32.pow(xcb as u32)
    }

    pub(crate) fn code_block_height(&self) -> u32 {
        let ycb = self
            .component_tile
            .component_info
            .coding_style
            .parameters
            .code_block_height;

        let ycb = if self.resolution > 0 {
            u8::min(ycb, self.precinct_exponent_y() - 1)
        } else {
            u8::min(ycb, self.precinct_exponent_y())
        };

        2_u32.pow(ycb as u32)
    }
}

pub(crate) struct TilePartHeader {
    pub(crate) tile_index: u16,
    pub(crate) tile_part_length: u32,
}

pub(crate) struct PptMarkerData<'a> {
    pub(crate) data: &'a [u8],
    pub(crate) sequence_idx: u8,
}

pub(crate) fn ppt_marker<'a>(reader: &mut BitReader<'a>) -> Option<PptMarkerData<'a>> {
    let length = reader.read_u16()?.checked_sub(2)?;
    let header_len = length.checked_sub(1)?;
    let sequence_idx = reader.read_byte()?;
    Some(PptMarkerData {
        data: reader.read_bytes(header_len as usize)?,
        sequence_idx,
    })
}

pub(crate) fn sot_marker(reader: &mut BitReader<'_>) -> Option<TilePartHeader> {
    let _ = reader.read_u16()?;

    let tile_index = reader.read_u16()?;
    let tile_part_length = reader.read_u32()?;

    let _tile_part_index = reader.read_byte()? as u16;
    let _num_tile_parts = reader.read_byte()?;

    Some(TilePartHeader {
        tile_index,
        tile_part_length,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::j2c::codestream::{
        CodeBlockStyle, CodingStyleComponent, CodingStyleDefault, CodingStyleFlags,
        CodingStyleParameters, QuantizationInfo, QuantizationStyle, SizeData, WaveletTransform,
    };

    #[test]
    fn tile_geometry_zero_offset() {
        let component_info = ComponentInfo {
            coding_style: CodingStyleComponent {
                flags: CodingStyleFlags::default(),
                parameters: CodingStyleParameters {
                    num_decomposition_levels: 0,
                    num_resolution_levels: 0,
                    code_block_width: 0,
                    code_block_height: 0,
                    code_block_style: CodeBlockStyle::default(),
                    transformation: WaveletTransform::Irreversible97,
                    precinct_exponents: vec![],
                },
            },
            quantization_info: QuantizationInfo {
                quantization_style: QuantizationStyle::NoQuantization,
                guard_bits: 0,
                step_sizes: vec![],
            },
            roi_shift: 0,
        };

        let size_data = SizeData {
            reference_grid_width: 1280,
            reference_grid_height: 720,
            tile_width: 396,
            tile_height: 297,
            num_components: 1,
        };

        assert_eq!(size_data.num_x_tiles(), 4);
        assert_eq!(size_data.num_y_tiles(), 3);
        assert_eq!(size_data.num_tiles(), Ok(12));

        let header = Header {
            size_data,
            global_coding_style: CodingStyleDefault {
                progression_order: ProgressionOrder::LayerResolutionComponentPosition,
                num_layers: 0,
                mct: false,
                component_parameters: component_info.coding_style.clone(),
            },
            component_infos: vec![],
            ppm_packets: vec![],
            progression_changes: vec![],
            levels: 0,
            strict: false,
            marker_budget: MarkerBudget::default(),
        };

        let tile_0_0 = Tile::new(0, &header);
        let coords_0_0 = ComponentTile::new(&tile_0_0, &component_info).rect;
        assert_eq!(coords_0_0, IntRect::from_ltrb(0, 0, 396, 297));

        let tile_3_2 = Tile::new(11, &header);
        let coords_3_2 = ComponentTile::new(&tile_3_2, &component_info).rect;
        assert_eq!(coords_3_2, IntRect::from_ltrb(1188, 594, 1280, 720));
    }
}
