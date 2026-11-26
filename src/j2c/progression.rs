use core::cmp::Ordering;
use std::collections::BTreeSet;
use std::vec;
use std::vec::Vec;

use super::codestream::{Header, ProgressionOrder};
use super::tile::{ComponentTile, ResolutionTile, Tile};
use crate::error::{DecodeError, Result};

#[derive(Default, Copy, Clone, Debug, PartialEq, Hash, Eq, PartialOrd, Ord)]
pub(crate) struct ProgressionData {
    pub(crate) layer_num: u8,
    pub(crate) resolution: u8,
    pub(crate) component: u8,
    pub(crate) precinct: u64,
}

pub(crate) struct IteratorInput<'a> {
    layers: (u8, u8),
    tile: &'a Tile,
    header: &'a Header,
    resolutions: (u8, u8),
    components: (u8, u8),
}

impl<'a> IteratorInput<'a> {
    pub(crate) fn new(tile: &'a Tile, header: &'a Header) -> Self {
        Self::new_with_custom_bounds(tile, header, (0, u8::MAX), (0, u8::MAX), (0, u8::MAX))
    }

    pub(crate) fn new_with_custom_bounds(
        tile: &'a Tile,
        header: &'a Header,
        mut resolutions: (u8, u8),
        mut layers: (u8, u8),
        mut components: (u8, u8),
    ) -> Self {
        let component_infos = tile.component_infos(header);
        let max_resolution = component_infos
            .iter()
            .map(|c| c.coding_style.parameters.num_resolution_levels)
            .max()
            .unwrap_or(0);
        let max_layer = tile.num_layers;
        let max_component = component_infos.len() as u8;

        resolutions.1 = resolutions.1.min(max_resolution);
        layers.1 = layers.1.min(max_layer);
        components.1 = components.1.min(max_component);

        Self {
            layers,
            tile,
            header,
            resolutions,
            components,
        }
    }

    fn min_layer(&self) -> u8 {
        self.layers.0
    }

    fn max_layer(&self) -> u8 {
        self.layers.1
    }

    fn min_resolution(&self) -> u8 {
        self.resolutions.0
    }

    fn total_max_resolution(&self) -> u8 {
        self.resolutions.1
    }

    fn max_resolution(&self, component_idx: u8) -> u8 {
        self.tile
            .component_infos(self.header)
            .get(component_idx as usize)
            .map_or(0, |c| {
                self.total_max_resolution().min(c.num_resolution_levels())
            })
    }

    fn min_comp(&self) -> u8 {
        self.components.0
    }

    fn max_comp(&self) -> u8 {
        self.components.1
    }

    fn is_empty(&self) -> bool {
        self.layers.0 >= self.layers.1
            || self.resolutions.0 >= self.resolutions.1
            || self.components.0 >= self.components.1
    }

    fn num_precincts(&self, component_idx: u8, resolution: u8) -> u64 {
        if component_idx >= self.max_comp() || resolution >= self.max_resolution(component_idx) {
            return 0;
        }

        let Some(component_info) = self
            .tile
            .component_infos(self.header)
            .get(component_idx as usize)
        else {
            return 0;
        };

        let component_tile = ComponentTile::new(self.tile, component_info);
        ResolutionTile::new(component_tile, resolution).num_precincts()
    }
}

pub(crate) enum ProgressionIterator<'a> {
    Lrcp(OrderIterator<'a>),
    Rlcp(OrderIterator<'a>),
    Position(PositionIterator),
    Poc(PocIterator<'a>),
}

impl Iterator for ProgressionIterator<'_> {
    type Item = ProgressionData;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Lrcp(iter) => iter.next_lrcp(),
            Self::Rlcp(iter) => iter.next_rlcp(),
            Self::Position(iter) => iter.next(),
            Self::Poc(iter) => iter.next(),
        }
    }
}

pub(crate) struct OrderIterator<'a> {
    input: IteratorInput<'a>,
    layer: u8,
    resolution: u8,
    component_idx: u8,
    num_precincts: u64,
    precinct: u64,
}

impl<'a> OrderIterator<'a> {
    fn new(input: IteratorInput<'a>) -> Self {
        let layer = input.min_layer();
        let resolution = input.min_resolution();
        let component_idx = input.min_comp();
        let num_precincts = input.num_precincts(component_idx, resolution);
        Self {
            input,
            layer,
            resolution,
            component_idx,
            num_precincts,
            precinct: 0,
        }
    }

    fn next_lrcp(&mut self) -> Option<ProgressionData> {
        if self.input.is_empty() || self.layer >= self.input.max_layer() {
            return None;
        }

        while self.precinct == self.num_precincts {
            self.precinct = 0;
            self.component_idx += 1;

            if self.component_idx == self.input.max_comp() {
                self.component_idx = self.input.min_comp();
                self.resolution += 1;

                if self.resolution == self.input.total_max_resolution() {
                    self.resolution = self.input.min_resolution();
                    self.layer += 1;

                    if self.layer == self.input.max_layer() {
                        return None;
                    }
                }
            }

            self.num_precincts = self
                .input
                .num_precincts(self.component_idx, self.resolution);
        }

        let data = ProgressionData {
            layer_num: self.layer,
            resolution: self.resolution,
            component: self.component_idx,
            precinct: self.precinct,
        };

        self.precinct += 1;

        Some(data)
    }

    fn next_rlcp(&mut self) -> Option<ProgressionData> {
        if self.input.is_empty() || self.resolution >= self.input.total_max_resolution() {
            return None;
        }

        while self.precinct == self.num_precincts {
            self.precinct = 0;
            self.component_idx += 1;

            if self.component_idx == self.input.max_comp() {
                self.component_idx = self.input.min_comp();
                self.layer += 1;

                if self.layer == self.input.max_layer() {
                    self.layer = self.input.min_layer();
                    self.resolution += 1;

                    if self.resolution == self.input.total_max_resolution() {
                        return None;
                    }
                }
            }

            self.num_precincts = self
                .input
                .num_precincts(self.component_idx, self.resolution);
        }

        let data = ProgressionData {
            layer_num: self.layer,
            resolution: self.resolution,
            component: self.component_idx,
            precinct: self.precinct,
        };

        self.precinct += 1;

        Some(data)
    }
}

struct PrecinctStore {
    resolution: u8,
    precinct_y: u32,
    precinct_x: u32,
    component_idx: u8,
    precinct_idx: u64,
}

pub(crate) struct PositionIterator {
    elements: Vec<PrecinctStore>,
    idx: usize,
    layer: u8,
    min_layer: u8,
    max_layer: u8,
}

impl Iterator for PositionIterator {
    type Item = ProgressionData;

    fn next(&mut self) -> Option<Self::Item> {
        if self.min_layer >= self.max_layer {
            return None;
        }

        let element = self.elements.get(self.idx)?;
        let data = ProgressionData {
            layer_num: self.layer,
            resolution: element.resolution,
            component: element.component_idx,
            precinct: element.precinct_idx,
        };

        self.layer += 1;
        if self.layer == self.max_layer {
            self.layer = self.min_layer;
            self.idx += 1;
        }

        Some(data)
    }
}

pub(crate) struct PocIterator<'a> {
    iterators: Vec<ProgressionIterator<'a>>,
    current: usize,
    included: BTreeSet<ProgressionData>,
}

impl Iterator for PocIterator<'_> {
    type Item = ProgressionData;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(iter) = self.iterators.get_mut(self.current) {
            if let Some(data) = iter.next() {
                if self.included.insert(data) {
                    return Some(data);
                }
            } else {
                self.current += 1;
            }
        }

        None
    }
}

pub(crate) fn progression_iterator<'a>(
    tile: &'a Tile,
    header: &'a Header,
) -> Result<ProgressionIterator<'a>> {
    let progression_changes = if tile.progression_changes.is_empty() {
        &header.progression_changes
    } else {
        &tile.progression_changes
    };

    if progression_changes.is_empty() {
        return progression_order_iterator(
            IteratorInput::new(tile, header),
            tile.progression_order,
        );
    }

    let clamp = |value: u16| u8::try_from(value).unwrap_or(u8::MAX);

    let iterators = progression_changes
        .iter()
        .map(|change| {
            let input = IteratorInput::new_with_custom_bounds(
                tile,
                header,
                (change.resolution_start, change.resolution_end),
                (0, clamp(change.layer_end)),
                (clamp(change.component_start), clamp(change.component_end)),
            );

            progression_order_iterator(input, change.progression_order)
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(ProgressionIterator::Poc(PocIterator {
        iterators,
        current: 0,
        included: BTreeSet::new(),
    }))
}

fn progression_order_iterator<'a>(
    input: IteratorInput<'a>,
    progression_order: ProgressionOrder,
) -> Result<ProgressionIterator<'a>> {
    Ok(match progression_order {
        ProgressionOrder::LayerResolutionComponentPosition => {
            ProgressionIterator::Lrcp(OrderIterator::new(input))
        }
        ProgressionOrder::ResolutionLayerComponentPosition => {
            ProgressionIterator::Rlcp(OrderIterator::new(input))
        }
        ProgressionOrder::ResolutionPositionComponentLayer => ProgressionIterator::Position(
            position_progression_common(input, |p, s| {
                p.resolution
                    .cmp(&s.resolution)
                    .then_with(|| p.precinct_y.cmp(&s.precinct_y))
                    .then_with(|| p.precinct_x.cmp(&s.precinct_x))
                    .then_with(|| p.component_idx.cmp(&s.component_idx))
                    .then_with(|| p.precinct_idx.cmp(&s.precinct_idx))
            })
            .ok_or(DecodeError::Malformed)?,
        ),
        ProgressionOrder::PositionComponentResolutionLayer => ProgressionIterator::Position(
            position_progression_common(input, |p, s| {
                p.precinct_y
                    .cmp(&s.precinct_y)
                    .then_with(|| p.precinct_x.cmp(&s.precinct_x))
                    .then_with(|| p.component_idx.cmp(&s.component_idx))
                    .then_with(|| p.resolution.cmp(&s.resolution))
                    .then_with(|| p.precinct_idx.cmp(&s.precinct_idx))
            })
            .ok_or(DecodeError::Malformed)?,
        ),
        ProgressionOrder::ComponentPositionResolutionLayer => ProgressionIterator::Position(
            position_progression_common(input, |p, s| {
                p.component_idx
                    .cmp(&s.component_idx)
                    .then_with(|| p.precinct_y.cmp(&s.precinct_y))
                    .then_with(|| p.precinct_x.cmp(&s.precinct_x))
                    .then_with(|| p.resolution.cmp(&s.resolution))
                    .then_with(|| p.precinct_idx.cmp(&s.precinct_idx))
            })
            .ok_or(DecodeError::Malformed)?,
        ),
    })
}

fn position_progression_common(
    input: IteratorInput<'_>,
    sort: impl FnMut(&PrecinctStore, &PrecinctStore) -> Ordering,
) -> Option<PositionIterator> {
    let component_infos = input.tile.component_infos(input.header);
    let mut elements = vec![];

    for component_idx in input.min_comp()..input.max_comp() {
        let component_info = component_infos.get(component_idx as usize)?;
        let component_tile = ComponentTile::new(input.tile, component_info);

        for resolution in input.min_resolution()..input.max_resolution(component_idx) {
            let resolution_tile = ResolutionTile::new(component_tile, resolution);

            elements.extend(resolution_tile.precincts()?.map(|d| PrecinctStore {
                precinct_y: d.r_y,
                precinct_x: d.r_x,
                component_idx,
                resolution,
                precinct_idx: d.idx,
            }));
        }
    }

    elements.sort_by(sort);

    Some(PositionIterator {
        elements,
        idx: 0,
        layer: input.min_layer(),
        min_layer: input.min_layer(),
        max_layer: input.max_layer(),
    })
}
