use super::codestream::{Header, ProgressionOrder};
use super::decode::{DecompositionStorage, TileDecompositions};
use super::rect::IntRect;
use super::tag_tree::TagTree;
use super::tile::{ResolutionTile, Tile};
use crate::check_cancelled;
use crate::error::{DecodeError, Result};
use core::iter;
use core::ops::Range;

pub(crate) fn can_stop_at_discard(tile: &Tile, header: &Header) -> bool {
    tile.progression_changes.is_empty()
        && header.progression_changes.is_empty()
        && matches!(
            tile.progression_order,
            ProgressionOrder::ResolutionLayerComponentPosition
                | ProgressionOrder::ResolutionPositionComponentLayer
        )
}

pub(crate) fn max_decoded_resolutions(
    tile: &Tile,
    header: &Header,
    skipped_resolution_levels: u8,
) -> Result<u8> {
    let mut max_res = 0_u8;
    for component_info in tile.component_infos(header) {
        let res = component_info
            .num_resolution_levels()
            .checked_sub(skipped_resolution_levels)
            .filter(|r| *r > 0)
            .ok_or(DecodeError::Malformed)?;
        max_res = max_res.max(res);
    }
    Ok(max_res)
}

pub(crate) fn build(
    tile: &Tile,
    header: &Header,
    storage: &mut DecompositionStorage,
    skipped_resolution_levels: u8,
    cancelled: &dyn Fn() -> bool,
) -> Result<()> {
    build_decompositions(tile, header, storage, skipped_resolution_levels, cancelled)
}

fn build_decompositions(
    tile: &Tile,
    header: &Header,
    storage: &mut DecompositionStorage,
    skipped_resolution_levels: u8,
    cancelled: &dyn Fn() -> bool,
) -> Result<()> {
    let stop_at_discard = can_stop_at_discard(tile, header);
    let max_res = max_decoded_resolutions(tile, header, skipped_resolution_levels)?;

    for component_tile in tile.component_tiles(header) {
        let comp_decoded_resolutions = component_tile
            .component_info
            .num_resolution_levels()
            .checked_sub(skipped_resolution_levels)
            .filter(|r| *r > 0)
            .ok_or(DecodeError::Malformed)?;
        let built_resolutions = if stop_at_discard {
            usize::from(max_res)
        } else {
            usize::from(component_tile.component_info.num_resolution_levels())
        };
        let d_start = storage.decompositions.len();
        let mut resolution_tiles = component_tile.resolution_tiles().take(built_resolutions);

        let build_sub_band = |sub_band_type: SubBandType,
                              resolution_tile: &ResolutionTile<'_>,
                              storage: &mut DecompositionStorage|
         -> Result<usize> {
            let sub_band_rect = resolution_tile.sub_band_rect(sub_band_type);
            let allocate_layers = resolution_tile.resolution < comp_decoded_resolutions;

            let precincts = build_precincts(
                resolution_tile,
                sub_band_rect,
                tile,
                allocate_layers,
                storage,
                cancelled,
            )?;

            let idx = storage.sub_bands.len();
            storage.sub_bands.push(SubBand {
                sub_band_type,
                rect: sub_band_rect,
                precincts,
                cb_height: resolution_tile.code_block_height().max(1),
            });

            Ok(idx)
        };

        let ll_resolution_tile = resolution_tiles.next().ok_or(DecodeError::Malformed)?;
        let first_ll_sub_band = build_sub_band(SubBandType::LowLow, &ll_resolution_tile, storage)?;

        for resolution_tile in resolution_tiles {
            let decomposition = Decomposition {
                sub_bands: [
                    build_sub_band(SubBandType::HighLow, &resolution_tile, storage)?,
                    build_sub_band(SubBandType::LowHigh, &resolution_tile, storage)?,
                    build_sub_band(SubBandType::HighHigh, &resolution_tile, storage)?,
                ],
                rect: resolution_tile.rect,
            };

            storage.decompositions.push(decomposition);
        }

        let d_end = storage.decompositions.len();

        storage.tile_decompositions.push(TileDecompositions {
            decompositions: d_start..d_end,
            first_ll_sub_band,
        });
    }

    Ok(())
}

fn build_precincts(
    resolution_tile: &ResolutionTile<'_>,
    sub_band_rect: IntRect,
    tile: &Tile,
    allocate_layers: bool,
    storage: &mut DecompositionStorage,
    cancelled: &dyn Fn() -> bool,
) -> Result<Range<usize>> {
    let start = storage.precincts.len();

    for precinct_data in resolution_tile.precincts().ok_or(DecodeError::Malformed)? {
        check_cancelled(cancelled)?;

        let precinct_rect = precinct_data.rect;

        let cb_width = resolution_tile.code_block_width();
        let cb_height = resolution_tile.code_block_height();

        let cb_x0 = (u32::max(precinct_rect.x0, sub_band_rect.x0) / cb_width) * cb_width;
        let cb_y0 = (u32::max(precinct_rect.y0, sub_band_rect.y0) / cb_height) * cb_height;
        let cb_x1 = (u32::min(precinct_rect.x1, sub_band_rect.x1).div_ceil(cb_width)) * cb_width;
        let cb_y1 = (u32::min(precinct_rect.y1, sub_band_rect.y1).div_ceil(cb_height)) * cb_height;

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

        let blocks = build_code_blocks(
            code_block_area,
            sub_band_rect,
            resolution_tile,
            code_blocks_x,
            code_blocks_y,
            tile,
            allocate_layers,
            storage,
            cancelled,
        )?;

        let code_inclusion_tree =
            TagTree::new(code_blocks_x, code_blocks_y, &mut storage.tag_tree_nodes);
        let zero_bitplane_tree =
            TagTree::new(code_blocks_x, code_blocks_y, &mut storage.tag_tree_nodes);

        storage.precincts.push(Precinct {
            code_blocks: blocks,
            code_inclusion_tree,
            zero_bitplane_tree,
        });
    }

    let end = storage.precincts.len();

    Ok(start..end)
}

fn build_code_blocks(
    code_block_area: IntRect,
    sub_band_rect: IntRect,
    tile_instance: &ResolutionTile<'_>,
    code_blocks_x: u32,
    code_blocks_y: u32,
    tile: &Tile,
    allocate_layers: bool,
    storage: &mut DecompositionStorage,
    cancelled: &dyn Fn() -> bool,
) -> Result<Range<usize>> {
    let mut y = code_block_area.y0;

    let code_block_width = tile_instance.code_block_width();
    let code_block_height = tile_instance.code_block_height();

    let start = storage.code_blocks.len();

    for y_idx in 0..code_blocks_y {
        check_cancelled(cancelled)?;

        let mut x = code_block_area.x0;

        for x_idx in 0..code_blocks_x {
            let area = IntRect::from_xywh(x, y, code_block_width, code_block_height)
                .intersect(sub_band_rect);

            let (layers_start, layers_end) = if allocate_layers {
                let start =
                    u32::try_from(storage.layers.len()).map_err(|_| DecodeError::TooLarge)?;
                storage
                    .layers
                    .extend(iter::repeat_n(Layer::EMPTY, tile.num_layers as usize));
                let end = u32::try_from(storage.layers.len()).map_err(|_| DecodeError::TooLarge)?;
                (start, end)
            } else {
                (0, 0)
            };

            storage.code_blocks.push(CodeBlock {
                x_idx,
                y_idx,
                rect: area,
                has_been_included: false,
                missing_bit_planes: 0,
                l_block: 3,
                number_of_coding_passes: 0,
                layers_start,
                layers_end,
                non_empty_layer_count: 0,
            });

            x += code_block_width;
        }

        y += code_block_height;
    }

    let end = storage.code_blocks.len();

    Ok(start..end)
}

pub(crate) struct Decomposition {
    pub(crate) sub_bands: [usize; 3],
    pub(crate) rect: IntRect,
}

#[derive(Clone)]
pub(crate) struct SubBand {
    pub(crate) sub_band_type: SubBandType,
    pub(crate) rect: IntRect,
    pub(crate) precincts: Range<usize>,
    pub(crate) cb_height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SubBandType {
    LowLow = 0,
    HighLow = 1,
    LowHigh = 2,
    HighHigh = 3,
}

#[derive(Clone)]
pub(crate) struct Precinct {
    pub(crate) code_blocks: Range<usize>,
    pub(crate) code_inclusion_tree: TagTree,
    pub(crate) zero_bitplane_tree: TagTree,
}

pub(crate) struct PrecinctData {
    pub(crate) r_x: u32,
    pub(crate) r_y: u32,
    pub(crate) rect: IntRect,
    pub(crate) idx: u64,
}

#[derive(Clone)]
pub(crate) struct CodeBlock {
    pub(crate) rect: IntRect,
    pub(crate) x_idx: u32,
    pub(crate) y_idx: u32,
    pub(crate) layers_start: u32,
    pub(crate) layers_end: u32,
    pub(crate) l_block: u32,
    pub(crate) has_been_included: bool,
    pub(crate) missing_bit_planes: u8,
    pub(crate) number_of_coding_passes: u8,
    pub(crate) non_empty_layer_count: u8,
}

impl CodeBlock {
    pub(crate) fn layers_range(&self) -> Range<usize> {
        self.layers_start as usize..self.layers_end as usize
    }
}

pub(crate) struct Segment {
    pub(crate) data_start: usize,
    pub(crate) data_length: u32,
    pub(crate) idx: u8,
    pub(crate) coding_pases: u8,
}

#[derive(Clone, Copy)]
pub(crate) struct Layer {
    pub(crate) start: u32,
    pub(crate) end: u32,
}

impl Layer {
    pub(crate) const EMPTY: Self = Self { start: 0, end: 0 };

    pub(crate) fn segments(self) -> Option<Range<usize>> {
        (self.start < self.end).then_some(self.start as usize..self.end as usize)
    }

    pub(crate) fn take(&mut self) -> Option<Range<usize>> {
        let range = self.segments();
        *self = Self::EMPTY;
        range
    }
}
