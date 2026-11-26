use std::vec;
use std::vec::Vec;

use super::build::{PrecinctData, SubBandType};
use super::codestream::{
    ComponentInfo, Header, ProgressionChange, ProgressionOrder, markers, skip_marker_segment,
};
use super::rect::IntRect;
use crate::error::{MarkerError, Result, TileError, ValidationError, bail, err};
use crate::j2c::codestream;
use crate::reader::BitReader;

#[derive(Clone, Debug)]
pub(crate) struct Tile<'a> {
    pub(crate) tile_parts: Vec<TilePart<'a>>,
    pub(crate) component_infos: Vec<ComponentInfo>,
    pub(crate) rect: IntRect,
    pub(crate) progression_order: ProgressionOrder,
    pub(crate) progression_changes: Vec<ProgressionChange>,
    pub(crate) num_layers: u8,
    pub(crate) mct: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct MergedTilePart<'a> {
    pub(crate) data: BitReader<'a>,
}

#[derive(Clone, Debug)]
pub(crate) struct SeparatedTilePart<'a> {
    pub(crate) headers: Vec<BitReader<'a>>,
    pub(crate) active_header_reader: usize,
    pub(crate) body: BitReader<'a>,
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
                if s.headers[s.active_header_reader].at_end()
                    && s.headers.len() - 1 > s.active_header_reader
                {
                    s.active_header_reader += 1;
                }

                &mut s.headers[s.active_header_reader]
            }
        }
    }

    pub(crate) fn body(&mut self) -> &mut BitReader<'a> {
        match self {
            TilePart::Merged(m) => &mut m.data,
            TilePart::Separated(s) => &mut s.body,
        }
    }
}

impl<'a> Tile<'a> {
    fn new(idx: u32, header: &Header) -> Self {
        let rect = {
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

            IntRect::from_ltrb(x0, y0, x1, y1)
        };

        Tile {
            tile_parts: vec![],
            rect,
            component_infos: header.component_infos.clone(),
            progression_order: header.global_coding_style.progression_order,
            progression_changes: vec![],
            mct: header.global_coding_style.mct,
            num_layers: header.global_coding_style.num_layers,
        }
    }

    pub(crate) fn component_tiles(&self) -> impl Iterator<Item = ComponentTile<'_>> {
        self.component_infos
            .iter()
            .map(|i| ComponentTile::new(self, i))
    }
}

pub(crate) fn parse<'a>(
    reader: &mut BitReader<'a>,
    main_header: &'a Header,
) -> Result<Vec<Tile<'a>>> {
    let tile_count = main_header.size_data.num_tiles()?;
    let mut tiles = (0..tile_count)
        .map(|idx| Tile::new(idx, main_header))
        .collect::<Vec<_>>();

    let mut tile_part_idx = 0;

    parse_tile_part(reader, main_header, &mut tiles, tile_part_idx)?;
    tile_part_idx += 1;

    while reader.peek_marker() == Some(markers::SOT) {
        parse_tile_part(reader, main_header, &mut tiles, tile_part_idx)?;
        tile_part_idx += 1;
    }

    if main_header.strict && reader.read_marker()? != markers::EOC {
        bail!(MarkerError::Expected("EOC"));
    }

    Ok(tiles)
}

fn parse_tile_part<'a>(
    reader: &mut BitReader<'a>,
    main_header: &'a Header,
    tiles: &mut [Tile<'a>],
    tile_part_idx: usize,
) -> Result<()> {
    if reader.read_marker()? != markers::SOT {
        bail!(MarkerError::Expected("SOT"));
    }

    let tile_part_header = sot_marker(reader).ok_or(MarkerError::ParseFailure("SOT"))?;

    if tile_part_header.tile_index as u32 >= main_header.size_data.num_tiles()? {
        bail!(TileError::InvalidIndex);
    }

    let data_len = if tile_part_header.tile_part_length == 0 {
        reader.tail().map(|d| d.len()).unwrap_or(0)
    } else {
        // Psot includes the 12-byte SOT marker segment.
        (tile_part_header.tile_part_length as usize)
            .checked_sub(12)
            .ok_or(TileError::Invalid)?
    };

    let start = reader.offset();

    let tile = &mut tiles[tile_part_header.tile_index as usize];

    let mut ppt_headers = vec![];

    loop {
        // Some files have tile-parts without an SOD marker.
        if reader.offset() - start >= data_len {
            return if main_header.strict {
                err!(MarkerError::Expected("SOD"))
            } else {
                Ok(())
            };
        }

        let Some(marker) = reader.peek_marker() else {
            return if main_header.strict {
                err!(MarkerError::Invalid)
            } else {
                Ok(())
            };
        };

        match marker {
            markers::SOD => {
                reader.read_marker()?;
                break;
            }
            markers::COD => {
                reader.read_marker()?;
                let cod = codestream::cod_marker(reader).ok_or(MarkerError::ParseFailure("COD"))?;

                tile.mct = cod.mct;
                tile.num_layers = cod.num_layers;
                tile.progression_order = cod.progression_order;

                for component in &mut tile.component_infos {
                    component.coding_style.flags.raw |= cod.component_parameters.flags.raw;
                    component.coding_style.parameters = cod.component_parameters.clone().parameters;
                }
            }
            markers::COC => {
                reader.read_marker()?;

                let (component_index, coc) =
                    codestream::coc_marker(reader).ok_or(MarkerError::ParseFailure("COC"))?;

                let old = tile
                    .component_infos
                    .get_mut(component_index as usize)
                    .ok_or(ValidationError::InvalidComponentMetadata)?;

                old.coding_style.parameters = coc.parameters;
                old.coding_style.flags.raw |= coc.flags.raw;
            }
            markers::QCD => {
                reader.read_marker()?;
                let qcd = codestream::qcd_marker(reader).ok_or(MarkerError::ParseFailure("QCD"))?;

                for component_info in &mut tile.component_infos {
                    component_info.quantization_info = qcd.clone();
                }
            }
            markers::QCC => {
                reader.read_marker()?;
                let (component_index, qcc) =
                    codestream::qcc_marker(reader).ok_or(MarkerError::ParseFailure("QCC"))?;

                tile.component_infos
                    .get_mut(component_index as usize)
                    .ok_or(ValidationError::InvalidComponentMetadata)?
                    .quantization_info = qcc.clone();
            }
            markers::EOC => break,
            markers::PPT => {
                if !main_header.ppm_packets.is_empty() {
                    bail!(TileError::PpmPptConflict);
                }

                reader.read_marker()?;
                ppt_headers.push(ppt_marker(reader).ok_or(MarkerError::ParseFailure("PPT"))?);
            }
            markers::RGN => {
                reader.read_marker()?;
                let (component_index, roi_shift) =
                    codestream::rgn_marker(reader).ok_or(MarkerError::ParseFailure("RGN"))?;

                tile.component_infos
                    .get_mut(component_index as usize)
                    .ok_or(ValidationError::InvalidComponentMetadata)?
                    .roi_shift = roi_shift;
            }
            markers::POC => {
                reader.read_marker()?;
                tile.progression_changes.extend(
                    codestream::poc_marker(reader).ok_or(MarkerError::ParseFailure("POC"))?,
                );
            }
            markers::PLT => {
                reader.read_marker()?;
                skip_marker_segment(reader).ok_or(MarkerError::ParseFailure("PLT"))?;
            }
            markers::COM => {
                reader.read_marker()?;
                skip_marker_segment(reader).ok_or(MarkerError::ParseFailure("COM"))?;
            }
            (0x30..=0x3F) => {
                reader.read_marker()?;
            }
            _ => {
                bail!(MarkerError::Unsupported);
            }
        }
    }

    let remaining_bytes = if let Some(len) = data_len.checked_sub(reader.offset() - start) {
        len
    } else {
        return if main_header.strict {
            err!(TileError::Invalid)
        } else {
            Ok(())
        };
    };

    ppt_headers.sort_by_key(|p1| p1.sequence_idx);
    let mut headers: Vec<_> = ppt_headers.iter().map(|i| BitReader::new(i.data)).collect();

    if let Some(ppm_marker) = main_header.ppm_packets.get(tile_part_idx) {
        headers.push(BitReader::new(ppm_marker));
    }

    let data = match reader.read_bytes(remaining_bytes) {
        Some(data) => data,
        None if !main_header.strict => {
            let data = reader.tail().ok_or(TileError::Invalid)?;
            reader.jump_to_end();
            data
        }
        None => bail!(TileError::Invalid),
    };

    let tile_part = if !headers.is_empty() {
        TilePart::Separated(SeparatedTilePart {
            headers,
            active_header_reader: 0,
            body: BitReader::new(data),
        })
    } else {
        TilePart::Merged(MergedTilePart {
            data: BitReader::new(data),
        })
    };

    tile.tile_parts.push(tile_part);

    Ok(())
}

#[derive(Debug, Copy, Clone)]
pub(crate) struct ComponentTile<'a> {
    pub(crate) tile: &'a Tile<'a>,
    pub(crate) component_info: &'a ComponentInfo,
    pub(crate) rect: IntRect,
}

impl<'a> ComponentTile<'a> {
    pub(crate) fn new(tile: &'a Tile<'a>, component_info: &'a ComponentInfo) -> Self {
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
            .precinct_exponents[self.resolution as usize]
            .0
    }

    fn precinct_exponent_y(&self) -> u8 {
        self.component_tile
            .component_info
            .coding_style
            .parameters
            .precinct_exponents[self.resolution as usize]
            .1
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

        // The spec only reduces PPx/PPy for code-blocks, but precincts need it too.
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

struct TilePartHeader {
    tile_index: u16,
    tile_part_length: u32,
}

struct PptMarkerData<'a> {
    data: &'a [u8],
    sequence_idx: u8,
}

fn ppt_marker<'a>(reader: &mut BitReader<'a>) -> Option<PptMarkerData<'a>> {
    let length = reader.read_u16()?.checked_sub(2)?;
    let header_len = length.checked_sub(1)?;
    let sequence_idx = reader.read_byte()?;
    Some(PptMarkerData {
        data: reader.read_bytes(header_len as usize)?,
        sequence_idx,
    })
}

fn sot_marker(reader: &mut BitReader<'_>) -> Option<TilePartHeader> {
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
            resolution_shrink_factor: 1,
        };

        assert_eq!(size_data.image_width(), 1280);
        assert_eq!(size_data.image_height(), 720);
        assert_eq!(size_data.num_x_tiles(), 4);
        assert_eq!(size_data.num_y_tiles(), 3);
        assert_eq!(size_data.num_tiles().unwrap(), 12);

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
            skipped_resolution_levels: 0,
            strict: false,
        };

        let tile_0_0 = Tile::new(0, &header);
        let coords_0_0 = ComponentTile::new(&tile_0_0, &component_info).rect;
        assert_eq!(coords_0_0, IntRect::from_ltrb(0, 0, 396, 297));

        let tile_3_2 = Tile::new(11, &header);
        let coords_3_2 = ComponentTile::new(&tile_3_2, &component_info).rect;
        assert_eq!(coords_3_2, IntRect::from_ltrb(1188, 594, 1280, 720));
    }
}
