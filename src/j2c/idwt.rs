use std::vec;
use std::vec::Vec;

use super::build::Decomposition;
use super::codestream::{ComponentInfo, WaveletTransform};
use super::decode::{DecodeCtx, DecodeParams, DecompositionStorage, decode_sub_band_strip};
use super::mct;
use super::rect::IntRect;
use crate::check_cancelled;
use crate::error::{DecodeError, Result};
use crate::math::{self, Level, SIMD_WIDTH, Simd, dispatch, f32x8};

const NEG_ALPHA: f32 = 1.586_134_3;
const NEG_BETA: f32 = 0.052_980_117;
const NEG_GAMMA: f32 = -0.882_911_1;
const NEG_DELTA: f32 = -0.443_506_87;
const KAPPA: f32 = 1.230_174_1;
const INV_KAPPA: f32 = 1.0 / KAPPA;
const MAX_LIFT_STEPS: usize = 4;

const FILTER_53R: FilterSpec = FilterSpec {
    scale_low: 1.0,
    scale_high: 1.0,
    steps: &[
        LiftStep {
            low: true,
            rule: LiftRule::FloorQuarterOffset,
        },
        LiftStep {
            low: false,
            rule: LiftRule::FloorHalf,
        },
    ],
};

const FILTER_97I: FilterSpec = FilterSpec {
    scale_low: KAPPA,
    scale_high: INV_KAPPA,
    steps: &[
        LiftStep {
            low: true,
            rule: LiftRule::MulAdd(NEG_DELTA),
        },
        LiftStep {
            low: false,
            rule: LiftRule::MulAdd(NEG_GAMMA),
        },
        LiftStep {
            low: true,
            rule: LiftRule::MulAdd(NEG_BETA),
        },
        LiftStep {
            low: false,
            rule: LiftRule::MulAdd(NEG_ALPHA),
        },
    ],
};

#[derive(Clone, Copy)]
enum LiftRule {
    MulAdd(f32),
    FloorQuarterOffset,
    FloorHalf,
}

#[derive(Clone, Copy)]
struct LiftStep {
    low: bool,
    rule: LiftRule,
}

#[derive(Clone, Copy)]
struct FilterSpec {
    scale_low: f32,
    scale_high: f32,
    steps: &'static [LiftStep],
}

impl FilterSpec {
    fn for_transform(transform: WaveletTransform) -> &'static Self {
        match transform {
            WaveletTransform::Reversible53 => &FILTER_53R,
            WaveletTransform::Irreversible97 => &FILTER_97I,
        }
    }
}

struct BandSource {
    sub_band_idx: usize,
    resolution: u8,
    y0: u32,
    y1: u32,
    cb_height: u32,
    width: usize,
    strip_y0: u32,
    strip_y1: u32,
    strip: Vec<f32>,
    next_y: usize,
}

impl BandSource {
    fn new(sub_band_idx: usize, resolution: u8, storage: &DecompositionStorage) -> Result<Self> {
        let sb = storage
            .sub_bands
            .get(sub_band_idx)
            .ok_or(DecodeError::Malformed)?;
        Ok(Self {
            sub_band_idx,
            resolution,
            y0: sb.rect.y0,
            y1: sb.rect.y1,
            cb_height: sb.cb_height,
            width: sb.rect.width() as usize,
            strip_y0: sb.rect.y0,
            strip_y1: sb.rect.y0,
            strip: Vec::new(),
            next_y: 0,
        })
    }

    #[inline(always)]
    fn advance_to(&mut self, v: usize, ctx: &mut DecodeCtx<'_>) -> Result<()> {
        if self.width == 0 {
            return Ok(());
        }
        let abs_y = self
            .y0
            .checked_add(u32::try_from(v).map_err(|_| DecodeError::Malformed)?)
            .ok_or(DecodeError::Malformed)?;
        if abs_y >= self.y1 {
            return Err(DecodeError::Malformed);
        }
        if abs_y < self.strip_y0 || abs_y >= self.strip_y1 {
            self.decode_strip(abs_y, ctx)?;
        }
        Ok(())
    }

    #[inline(always)]
    fn row(&self, v: usize) -> Result<&[f32]> {
        if self.width == 0 {
            return Ok(&[]);
        }
        let abs_y = self
            .y0
            .checked_add(u32::try_from(v).map_err(|_| DecodeError::Malformed)?)
            .ok_or(DecodeError::Malformed)?;
        let local_y = abs_y
            .checked_sub(self.strip_y0)
            .ok_or(DecodeError::Malformed)? as usize;
        self.strip
            .get(local_y * self.width..)
            .and_then(|s| s.get(..self.width))
            .ok_or(DecodeError::Malformed)
    }

    #[inline(always)]
    fn next_row(&mut self, ctx: &mut DecodeCtx<'_>) -> Result<&[f32]> {
        let v = self.next_y;
        self.next_y += 1;
        self.advance_to(v, ctx)?;
        self.row(v)
    }

    #[cold]
    fn decode_strip(&mut self, abs_y: u32, ctx: &mut DecodeCtx<'_>) -> Result<()> {
        let next_grid = (abs_y / self.cb_height + 1).saturating_mul(self.cb_height);
        let abs_y_end = next_grid.min(self.y1);
        decode_sub_band_strip(
            ctx,
            self.sub_band_idx,
            self.resolution,
            abs_y..abs_y_end,
            &mut self.strip,
        )?;
        self.strip_y0 = abs_y;
        self.strip_y1 = abs_y_end;
        Ok(())
    }
}

struct RowRing {
    data: Vec<f32>,
    width: usize,
    rows: usize,
}

impl RowRing {
    fn new(width: usize, rows: usize) -> Result<Self> {
        let len = width.checked_mul(rows).ok_or(DecodeError::TooLarge)?;
        Ok(Self {
            data: vec![0.0; len],
            width,
            rows,
        })
    }

    #[inline(always)]
    fn slot(&self, row: usize) -> usize {
        row % self.rows
    }

    #[inline(always)]
    fn row(&self, row: usize) -> Result<&[f32]> {
        let offset = self.slot(row) * self.width;
        self.data
            .get(offset..)
            .and_then(|s| s.get(..self.width))
            .ok_or(DecodeError::Malformed)
    }

    #[inline(always)]
    fn row_mut(&mut self, row: usize) -> Result<&mut [f32]> {
        let offset = self.slot(row) * self.width;
        self.data
            .get_mut(offset..)
            .and_then(|s| s.get_mut(..self.width))
            .ok_or(DecodeError::Malformed)
    }

    #[inline(always)]
    fn load_row<S: Simd>(
        &mut self,
        simd: S,
        layout: LevelLayout,
        row_idx: usize,
        bands: (&[f32], &[f32]),
        ctx: &mut DecodeCtx<'_>,
    ) -> Result<()> {
        let dst = self.row_mut(row_idx)?;
        let mut horiz = HorizontalFilter::new(
            layout.spec,
            layout.low_first_x,
            layout.num_low_x,
            layout.num_high_x,
            ctx,
        )?;
        horiz.filter_row(simd, bands.0, bands.1, dst)?;

        let is_low = row_idx.is_multiple_of(2) == layout.low_first_y;
        let factor = if layout.height == 1 {
            if is_low { 1.0 } else { 0.5 }
        } else if is_low {
            layout.spec.scale_low
        } else {
            layout.spec.scale_high
        };
        if factor != 1.0 {
            self.scale_row(simd, row_idx, factor)?;
        }
        Ok(())
    }

    #[inline(always)]
    fn scale_row<S: Simd>(&mut self, simd: S, row: usize, factor: f32) -> Result<()> {
        if self.width == 0 {
            return Ok(());
        }
        scale(simd, self.row_mut(row)?, factor);
        Ok(())
    }

    #[inline(always)]
    fn lift_row<S: Simd>(
        &mut self,
        simd: S,
        row: usize,
        height: usize,
        rule: LiftRule,
    ) -> Result<()> {
        if self.width == 0 {
            return Ok(());
        }
        let above_slot = self.slot(mirror_above(row));
        let below_slot = self.slot(mirror_below(row, height));
        let slot = self.slot(row);
        let width = self.width;

        let (rows_before, rest) = self
            .data
            .split_at_mut_checked(slot * width)
            .ok_or(DecodeError::Malformed)?;
        let (current, rows_after) = rest
            .split_at_mut_checked(width)
            .ok_or(DecodeError::Malformed)?;
        let neighbor = |s: usize| -> Result<&[f32]> {
            if s < slot {
                rows_before
                    .get(s * width..)
                    .and_then(|r| r.get(..width))
                    .ok_or(DecodeError::Malformed)
            } else {
                let rel = s.checked_sub(slot + 1).ok_or(DecodeError::Malformed)?;
                rows_after
                    .get(rel * width..)
                    .and_then(|r| r.get(..width))
                    .ok_or(DecodeError::Malformed)
            }
        };

        apply_lift_rule(
            simd,
            rule,
            current,
            neighbor(above_slot)?,
            neighbor(below_slot)?,
        );
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct LevelLayout {
    spec: &'static FilterSpec,
    height: usize,
    low_first_x: bool,
    low_first_y: bool,
    num_low_x: usize,
    num_high_x: usize,
}

struct LevelSynthesis {
    hl: BandSource,
    lh: BandSource,
    hh: BandSource,
    layout: LevelLayout,
    ring: RowRing,
    loaded: usize,
    next_step_row: [usize; MAX_LIFT_STEPS],
    next_y: usize,
}

impl LevelSynthesis {
    fn new(
        decomposition: &Decomposition,
        resolution: u8,
        transform: WaveletTransform,
        storage: &DecompositionStorage,
    ) -> Result<Self> {
        let IntRect {
            x0: u0,
            x1: u1,
            y0: v0,
            ..
        } = decomposition.rect;
        let width = decomposition.rect.width() as usize;
        let height = decomposition.rect.height() as usize;
        let low_first_x = u0.is_multiple_of(2);
        let low_first_y = v0.is_multiple_of(2);
        let spec = FilterSpec::for_transform(transform);
        let ring_rows = height.min(spec.steps.len() + 2);

        let mut next_step_row = [0_usize; MAX_LIFT_STEPS];
        for (slot, step) in next_step_row.iter_mut().zip(spec.steps) {
            *slot = usize::from(step.low != low_first_y);
        }

        let [hl_idx, lh_idx, hh_idx] = decomposition.sub_bands;
        Ok(Self {
            hl: BandSource::new(hl_idx, resolution, storage)?,
            lh: BandSource::new(lh_idx, resolution, storage)?,
            hh: BandSource::new(hh_idx, resolution, storage)?,
            layout: LevelLayout {
                spec,
                height,
                low_first_x,
                low_first_y,
                num_low_x: (u1.div_ceil(2) - u0.div_ceil(2)) as usize,
                num_high_x: (u1 / 2 - u0 / 2) as usize,
            },
            ring: RowRing::new(width, ring_rows)?,
            loaded: 0,
            next_step_row,
            next_y: 0,
        })
    }

    #[inline(always)]
    fn is_ready(&self, y: usize) -> bool {
        if self.layout.height <= 1 {
            return self.loaded > y;
        }
        let is_low = y.is_multiple_of(2) == self.layout.low_first_y;
        let final_step = self.layout.spec.steps.len() - if is_low { 2 } else { 1 };
        self.next_step_row
            .get(final_step)
            .is_some_and(|&next| next > y)
    }

    #[inline(always)]
    fn next_loaded_is_low(&self) -> bool {
        self.loaded.is_multiple_of(2) == self.layout.low_first_y
    }

    #[inline(always)]
    fn take_ready_row(&mut self) -> Result<&[f32]> {
        let y = self.next_y;
        if y >= self.layout.height {
            return Err(DecodeError::Malformed);
        }
        self.next_y += 1;
        self.ring.row(y)
    }

    #[inline(always)]
    fn load_low_row<S: Simd>(
        &mut self,
        simd: S,
        low_src: &[f32],
        ctx: &mut DecodeCtx<'_>,
    ) -> Result<()> {
        let row_idx = self.loaded;
        if row_idx >= self.layout.height {
            return Err(DecodeError::Malformed);
        }
        self.loaded += 1;
        let high_src = self.hl.next_row(ctx)?;
        self.ring
            .load_row(simd, self.layout, row_idx, (low_src, high_src), ctx)
    }

    #[inline(always)]
    fn load_high_row<S: Simd>(&mut self, simd: S, ctx: &mut DecodeCtx<'_>) -> Result<()> {
        let row_idx = self.loaded;
        if row_idx >= self.layout.height {
            return Err(DecodeError::Malformed);
        }
        self.loaded += 1;
        let low_src = self.lh.next_row(ctx)?;
        let high_src = self.hh.next_row(ctx)?;
        self.ring
            .load_row(simd, self.layout, row_idx, (low_src, high_src), ctx)
    }

    #[inline(always)]
    fn advance_lifting<S: Simd>(&mut self, simd: S) -> Result<()> {
        let height = self.layout.height;
        if height <= 1 {
            return Ok(());
        }
        for (k, step) in self.layout.spec.steps.iter().enumerate() {
            while let Some(&row) = self.next_step_row.get(k) {
                if row >= height {
                    break;
                }
                let below = mirror_below(row, height);
                let ready = if k == 0 {
                    self.loaded > row.max(below)
                } else {
                    self.next_step_row
                        .get(k - 1)
                        .is_some_and(|&prev_next| prev_next > below)
                };
                if !ready {
                    break;
                }
                self.ring.lift_row(simd, row, height, step.rule)?;
                if let Some(slot) = self.next_step_row.get_mut(k) {
                    *slot = row + 2;
                }
            }
        }
        Ok(())
    }
}

struct ComponentSynthesis {
    ll: BandSource,
    levels: Vec<LevelSynthesis>,
}

impl ComponentSynthesis {
    fn new(
        storage: &DecompositionStorage,
        component_idx: usize,
        component_info: &ComponentInfo,
        skipped_resolution_levels: u8,
    ) -> Result<Self> {
        let tile_decompositions = storage
            .tile_decompositions
            .get(component_idx)
            .ok_or(DecodeError::Malformed)?;
        let decompositions = storage
            .decompositions
            .get(tile_decompositions.decompositions.clone())
            .ok_or(DecodeError::Malformed)?;
        let keep = usize::from(
            component_info
                .num_resolution_levels()
                .saturating_sub(skipped_resolution_levels)
                .saturating_sub(1),
        );
        let decompositions = decompositions.get(..keep).ok_or(DecodeError::Malformed)?;

        let ll = BandSource::new(tile_decompositions.first_ll_sub_band, 0, storage)?;
        let transform = component_info.wavelet_transform();
        let mut levels = Vec::with_capacity(decompositions.len());
        for (idx, decomposition) in decompositions.iter().enumerate() {
            let resolution = (idx + 1) as u8;
            levels.push(LevelSynthesis::new(
                decomposition,
                resolution,
                transform,
                storage,
            )?);
        }
        Ok(Self { ll, levels })
    }

    #[inline(always)]
    fn advance_to<S: Simd>(&mut self, simd: S, y: usize, ctx: &mut DecodeCtx<'_>) -> Result<()> {
        let Some(top) = self.levels.len().checked_sub(1) else {
            return self.ll.advance_to(y, ctx);
        };
        while !self
            .levels
            .get(top)
            .ok_or(DecodeError::Malformed)?
            .is_ready(y)
        {
            let mut cur = top;
            while cur > 0 {
                let cur_level = self.levels.get(cur).ok_or(DecodeError::Malformed)?;
                if !cur_level.next_loaded_is_low() {
                    break;
                }
                let child = self.levels.get(cur - 1).ok_or(DecodeError::Malformed)?;
                if child.is_ready(child.next_y) {
                    break;
                }
                cur -= 1;
            }
            self.step_level(simd, cur, ctx)?;
        }
        Ok(())
    }

    #[inline(always)]
    fn row(&self, y: usize) -> Result<&[f32]> {
        match self.levels.last() {
            Some(top) => top.ring.row(y),
            None => self.ll.row(y),
        }
    }

    #[inline(always)]
    fn step_level<S: Simd>(
        &mut self,
        simd: S,
        level_idx: usize,
        ctx: &mut DecodeCtx<'_>,
    ) -> Result<()> {
        if level_idx == 0 {
            let level = self.levels.first_mut().ok_or(DecodeError::Malformed)?;
            if level.next_loaded_is_low() {
                let low_src = self.ll.next_row(ctx)?;
                level.load_low_row(simd, low_src, ctx)?;
            } else {
                level.load_high_row(simd, ctx)?;
            }
            level.advance_lifting(simd)
        } else {
            let (lower, upper) = self
                .levels
                .split_at_mut_checked(level_idx)
                .ok_or(DecodeError::Malformed)?;
            let child = lower.last_mut().ok_or(DecodeError::Malformed)?;
            let level = upper.first_mut().ok_or(DecodeError::Malformed)?;
            if level.next_loaded_is_low() {
                let low_src = child.take_ready_row()?;
                level.load_low_row(simd, low_src, ctx)?;
            } else {
                level.load_high_row(simd, ctx)?;
            }
            level.advance_lifting(simd)
        }
    }
}

struct HorizontalFilter<'a> {
    spec: &'static FilterSpec,
    low_first: bool,
    num_low: usize,
    num_high: usize,
    low: &'a mut [f32],
    high: &'a mut [f32],
}

impl<'a> HorizontalFilter<'a> {
    #[inline(always)]
    fn new(
        spec: &'static FilterSpec,
        low_first: bool,
        num_low: usize,
        num_high: usize,
        ctx: &'a mut DecodeCtx<'_>,
    ) -> Result<Self> {
        let low_len = num_low + 2;
        if ctx.low_row.len() < low_len {
            ctx.low_row.resize(low_len, 0.0);
        }
        let high_len = num_high + 2;
        if ctx.high_row.len() < high_len {
            ctx.high_row.resize(high_len, 0.0);
        }
        let low = ctx
            .low_row
            .get_mut(..low_len)
            .ok_or(DecodeError::Malformed)?;
        let high = ctx
            .high_row
            .get_mut(..high_len)
            .ok_or(DecodeError::Malformed)?;
        Ok(Self {
            spec,
            low_first,
            num_low,
            num_high,
            low,
            high,
        })
    }

    #[inline(always)]
    fn filter_row<S: Simd>(
        &mut self,
        simd: S,
        low_src: &[f32],
        high_src: &[f32],
        row: &mut [f32],
    ) -> Result<()> {
        let total = self.num_low + self.num_high;
        if total == 0 {
            return Ok(());
        }
        if low_src.len() != self.num_low || high_src.len() != self.num_high {
            return Err(DecodeError::Malformed);
        }

        let low_dst = self
            .low
            .get_mut(1..=self.num_low)
            .ok_or(DecodeError::Malformed)?;
        low_dst.copy_from_slice(low_src);

        let high_dst = self
            .high
            .get_mut(1..=self.num_high)
            .ok_or(DecodeError::Malformed)?;
        high_dst.copy_from_slice(high_src);

        if total == 1 {
            if let Some(dst) = self.high.get_mut(1..=self.num_high) {
                scale(simd, dst, 0.5);
            }
        } else {
            if self.spec.scale_low != 1.0
                && let Some(dst) = self.low.get_mut(1..=self.num_low)
            {
                scale(simd, dst, self.spec.scale_low);
            }
            if self.spec.scale_high != 1.0
                && let Some(dst) = self.high.get_mut(1..=self.num_high)
            {
                scale(simd, dst, self.spec.scale_high);
            }
            for step in self.spec.steps {
                if step.low {
                    lift_band(
                        simd,
                        self.low,
                        self.high,
                        usize::from(self.low_first),
                        step.rule,
                    );
                } else {
                    lift_band(
                        simd,
                        self.high,
                        self.low,
                        usize::from(!self.low_first),
                        step.rule,
                    );
                }
            }
        }

        let low = self
            .low
            .get(1..=self.num_low)
            .ok_or(DecodeError::Malformed)?;
        let high = self
            .high
            .get(1..=self.num_high)
            .ok_or(DecodeError::Malformed)?;
        if self.low_first {
            interleave_row(simd, low, high, row);
        } else {
            interleave_row(simd, high, low, row);
        }
        Ok(())
    }
}

pub(crate) fn apply_tile<'a>(
    ctx: &mut DecodeCtx<'a>,
    tile_rect: IntRect,
    component_infos: &'a [ComponentInfo],
    tile_mct: Option<WaveletTransform>,
    params: DecodeParams,
    out: &mut [u8],
) -> Result<()> {
    let width = tile_rect.width() as usize;
    let height = tile_rect.height() as usize;
    if width == 0 || height == 0 {
        return Ok(());
    }

    let image_width = params.width as usize;
    let x0 = tile_rect.x0 as usize;
    let y0 = tile_rect.y0 as usize;
    if x0.checked_add(width).is_none_or(|end| end > image_width)
        || y0
            .checked_add(height)
            .is_none_or(|end| end > params.height as usize)
    {
        return Err(DecodeError::Malformed);
    }

    let num_components = component_infos.len();
    let mut components = Vec::with_capacity(num_components);
    for (idx, component_info) in component_infos.iter().enumerate() {
        check_cancelled(ctx.cancelled)?;
        components.push(ComponentSynthesis::new(
            ctx.storage,
            idx,
            component_info,
            params.skipped_resolution_levels,
        )?);
    }

    let half_cap = width.div_ceil(2) + 2;
    if ctx.low_row.len() < half_cap {
        ctx.low_row.resize(half_cap, 0.0);
    }
    if ctx.high_row.len() < half_cap {
        ctx.high_row.resize(half_cap, 0.0);
    }

    dispatch!(Level::new(), simd => {
        for y in 0..height {
            check_cancelled(ctx.cancelled)?;
            for (component, component_info) in components.iter_mut().zip(component_infos) {
                ctx.component_info = component_info;
                component.advance_to(simd, y, ctx)?;
            }
            let row_start = ((y0 + y) * image_width + x0) * num_components;
            let row_len = width * num_components;
            let out_row = out
                .get_mut(row_start..)
                .and_then(|s| s.get_mut(..row_len))
                .ok_or(DecodeError::Malformed)?;
            write_synthesized_row(simd, &components, y, tile_mct, out_row)?;
        }
        Ok(())
    })
}

#[inline(always)]
fn write_synthesized_row<S: Simd>(
    simd: S,
    components: &[ComponentSynthesis],
    y: usize,
    tile_mct: Option<WaveletTransform>,
    out_row: &mut [u8],
) -> Result<()> {
    match components {
        [s0] => mct::write_row(simd, &[s0.row(y)?], tile_mct, out_row),
        [s0, s1] => mct::write_row(simd, &[s0.row(y)?, s1.row(y)?], tile_mct, out_row),
        [s0, s1, s2] => mct::write_row(
            simd,
            &[s0.row(y)?, s1.row(y)?, s2.row(y)?],
            tile_mct,
            out_row,
        ),
        [s0, s1, s2, s3] => mct::write_row(
            simd,
            &[s0.row(y)?, s1.row(y)?, s2.row(y)?, s3.row(y)?],
            tile_mct,
            out_row,
        ),
        _ => return Err(DecodeError::Malformed),
    }
    Ok(())
}

#[inline(always)]
fn mirror_above(row: usize) -> usize {
    if row == 0 { 1 } else { row - 1 }
}

#[inline(always)]
fn mirror_below(row: usize, height: usize) -> usize {
    if row + 1 < height { row + 1 } else { row - 1 }
}

#[inline(always)]
fn lift_band<S: Simd>(
    simd: S,
    target: &mut [f32],
    source: &mut [f32],
    offset: usize,
    rule: LiftRule,
) {
    if let [first, second, ..] = source {
        *first = *second;
    }
    if let [.., prev, last] = source {
        *last = *prev;
    }

    let Some(len) = target.len().checked_sub(2) else {
        return;
    };
    let Some(target) = target.get_mut(1..).and_then(|s| s.get_mut(..len)) else {
        return;
    };
    let Some(left) = source.get(1 - offset..).and_then(|s| s.get(..len)) else {
        return;
    };
    let Some(right) = source.get(2 - offset..).and_then(|s| s.get(..len)) else {
        return;
    };

    apply_lift_rule(simd, rule, target, left, right);
}

#[inline(always)]
fn apply_lift_rule<S: Simd>(
    simd: S,
    rule: LiftRule,
    target: &mut [f32],
    left: &[f32],
    right: &[f32],
) {
    match rule {
        LiftRule::MulAdd(coeff) => {
            let coeff_simd = f32x8::splat(simd, coeff);
            lift_samples(
                simd,
                target,
                left,
                right,
                #[inline(always)]
                |s, l, r| (l + r).mul_add(coeff_simd, s),
                #[inline(always)]
                |s, l, r| math::mul_add(l + r, coeff, s),
            );
        }
        LiftRule::FloorQuarterOffset => {
            lift_samples(
                simd,
                target,
                left,
                right,
                #[inline(always)]
                |s, l, r| s - ((l + r + 2.0) * 0.25).floor(),
                #[inline(always)]
                |s, l, r| s - math::mul_add(l + r, 0.25, 0.5).floor(),
            );
        }
        LiftRule::FloorHalf => {
            lift_samples(
                simd,
                target,
                left,
                right,
                #[inline(always)]
                |s, l, r| s + ((l + r) * 0.5).floor(),
                #[inline(always)]
                |s, l, r| s + ((l + r) * 0.5).floor(),
            );
        }
    }
}

#[inline(always)]
fn interleave_row<S: Simd>(simd: S, first: &[f32], second: &[f32], output: &mut [f32]) {
    let num_pairs = first.len().min(second.len());
    let Some(first_pairs) = first.get(..num_pairs) else {
        return;
    };
    let Some(second_pairs) = second.get(..num_pairs) else {
        return;
    };
    let Some(out_pairs) = output.get_mut(..num_pairs * 2) else {
        return;
    };
    let (first_chunks, first_rest) = first_pairs.as_chunks::<SIMD_WIDTH>();
    let (second_chunks, second_rest) = second_pairs.as_chunks::<SIMD_WIDTH>();
    let (output_chunks, output_rest) = out_pairs.as_chunks_mut::<{ 2 * SIMD_WIDTH }>();

    for ((f, s), out) in first_chunks.iter().zip(second_chunks).zip(output_chunks) {
        let f = f32x8::from_slice(simd, f);
        let s = f32x8::from_slice(simd, s);
        let (low, high) = out.split_at_mut(SIMD_WIDTH);
        f.zip_low(s).store(low);
        f.zip_high(s).store(high);
    }

    let (output_pairs, _) = output_rest.as_chunks_mut::<2>();
    for ((f, s), out) in first_rest.iter().zip(second_rest).zip(output_pairs) {
        *out = [*f, *s];
    }

    if let (Some(&val), Some(dst)) = (first.get(num_pairs), output.get_mut(num_pairs * 2)) {
        *dst = val;
    }
}

#[inline(always)]
fn lift_samples<S: Simd>(
    simd: S,
    target: &mut [f32],
    left: &[f32],
    right: &[f32],
    f_simd: impl Fn(f32x8<S>, f32x8<S>, f32x8<S>) -> f32x8<S>,
    f_scalar: impl Fn(f32, f32, f32) -> f32,
) {
    let (target_chunks, target_rest) = target.as_chunks_mut::<SIMD_WIDTH>();
    let (left_chunks, left_rest) = left.as_chunks::<SIMD_WIDTH>();
    let (right_chunks, right_rest) = right.as_chunks::<SIMD_WIDTH>();

    for ((t, l), r) in target_chunks.iter_mut().zip(left_chunks).zip(right_chunks) {
        let s = f32x8::from_slice(simd, t);
        let l = f32x8::from_slice(simd, l);
        let r = f32x8::from_slice(simd, r);
        f_simd(s, l, r).store(t);
    }

    for ((t, l), r) in target_rest.iter_mut().zip(left_rest).zip(right_rest) {
        *t = f_scalar(*t, *l, *r);
    }
}

#[inline(always)]
fn scale<S: Simd>(simd: S, samples: &mut [f32], factor: f32) {
    let factor_simd = f32x8::splat(simd, factor);
    let (chunks, rest) = samples.as_chunks_mut::<SIMD_WIDTH>();

    for chunk in chunks {
        (f32x8::from_slice(simd, chunk) * factor_simd).store(chunk);
    }

    for sample in rest {
        *sample *= factor;
    }
}
