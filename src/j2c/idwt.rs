use core::mem;
use std::vec;
use std::vec::Vec;

use super::bitplane::{BitPlaneDecodeBuffers, BitPlaneDecodeContext};
use super::build::Decomposition;
use super::codestream::{ComponentInfo, WaveletTransform};
use super::decode::{DecodeParams, DecompositionStorage, decode_sub_band_strip};
use super::mct;
use super::rect::IntRect;
use crate::cancel::Cancel;
use crate::error::{DecodeError, Result};
use crate::j2c::Header;
use crate::math::{self, Level, SIMD_WIDTH, Simd, dispatch, f32x8};

const NEG_ALPHA: f32 = 1.586_134_3;
const NEG_BETA: f32 = 0.052_980_117;
const NEG_GAMMA: f32 = -0.882_911_1;
const NEG_DELTA: f32 = -0.443_506_87;
const KAPPA: f32 = 1.230_174_1;
const INV_KAPPA: f32 = 1.0 / KAPPA;

#[derive(Default)]
struct IntermediateScratch {
    buffer: Vec<f32>,
    aux_buffer: Vec<f32>,
    low_row: Vec<f32>,
    high_row: Vec<f32>,
    strip_a: Vec<f32>,
    strip_b: Vec<f32>,
}

enum ComponentStage {
    Direct(DirectStage),
    LastDwt(LastStage),
}

impl ComponentStage {
    #[inline(always)]
    fn advance_to<S: Simd>(
        &mut self,
        simd: S,
        y: usize,
        codestream: &[u8],
        storage: &DecompositionStorage,
        component_info: &ComponentInfo,
        header: &Header,
        max_layers: Option<u8>,
        low_row: &mut Vec<f32>,
        high_row: &mut Vec<f32>,
        bit_plane_ctx: &mut BitPlaneDecodeContext,
        bit_plane_buffers: &mut BitPlaneDecodeBuffers,
        cancel: Cancel<'_>,
    ) -> Result<()> {
        match self {
            Self::Direct(stage) => stage.advance_to(
                y,
                codestream,
                storage,
                component_info,
                header,
                max_layers,
                bit_plane_ctx,
                bit_plane_buffers,
                cancel,
            ),
            Self::LastDwt(stage) => stage.advance_to(
                simd,
                y,
                codestream,
                storage,
                component_info,
                header,
                max_layers,
                low_row,
                high_row,
                bit_plane_ctx,
                bit_plane_buffers,
                cancel,
            ),
        }
    }

    #[inline(always)]
    fn row(&self, y: usize) -> Result<&[f32]> {
        match self {
            Self::Direct(stage) => stage.row(y),
            Self::LastDwt(stage) => stage.row(y),
        }
    }
}

struct DirectStage {
    sub_band_idx: usize,
    y0: u32,
    y1: u32,
    cb_height: u32,
    width: usize,
    strip_y0: u32,
    strip_y1: u32,
    strip: Vec<f32>,
}

impl DirectStage {
    fn advance_to(
        &mut self,
        y: usize,
        codestream: &[u8],
        storage: &DecompositionStorage,
        component_info: &ComponentInfo,
        header: &Header,
        max_layers: Option<u8>,
        bit_plane_ctx: &mut BitPlaneDecodeContext,
        bit_plane_buffers: &mut BitPlaneDecodeBuffers,
        cancel: Cancel<'_>,
    ) -> Result<()> {
        let abs_y = self.y0 + y as u32;
        if abs_y < self.strip_y1 && abs_y >= self.strip_y0 {
            return Ok(());
        }
        let next_grid = (abs_y / self.cb_height + 1).saturating_mul(self.cb_height);
        let abs_y_end = next_grid.min(self.y1);
        decode_sub_band_strip(
            codestream,
            self.sub_band_idx,
            0,
            component_info,
            bit_plane_ctx,
            bit_plane_buffers,
            storage,
            header,
            max_layers,
            abs_y,
            abs_y_end,
            &mut self.strip,
            cancel,
        )?;
        self.strip_y0 = abs_y;
        self.strip_y1 = abs_y_end;
        Ok(())
    }

    fn row(&self, y: usize) -> Result<&[f32]> {
        let local_y = (self.y0 + y as u32 - self.strip_y0) as usize;
        self.strip
            .get(local_y * self.width..)
            .and_then(|s| s.get(..self.width))
            .ok_or(DecodeError::Malformed)
    }
}

enum WindowState {
    Small {
        buf: Vec<f32>,
        computed: bool,
    },
    Ring53r {
        ring: Vec<f32>,
        next_step: usize,
        ready_until: usize,
    },
    Ring97i {
        ring: Vec<f32>,
        next_step: usize,
        ready_until: usize,
    },
}

struct LastStage {
    ll_plane: Vec<f32>,
    rect: IntRect,
    resolution: u8,
    sub_bands: [usize; 3],
    transform: WaveletTransform,
    low_first_x: bool,
    low_first_y: bool,
    num_low_x: usize,
    num_high_x: usize,
    hl_y0: u32,
    hl_y1: u32,
    hl_cb_height: u32,
    hl_strip_y0: u32,
    hl_strip_y1: u32,
    hl_strip: Vec<f32>,
    lh_y0: u32,
    lh_y1: u32,
    lh_cb_height: u32,
    lh_strip_y0: u32,
    lh_strip_y1: u32,
    lh_strip: Vec<f32>,
    hh_strip: Vec<f32>,
    window: WindowState,
}

impl LastStage {
    #[inline(always)]
    fn load_row<S: Simd>(
        &mut self,
        simd: S,
        row_idx: usize,
        dst: &mut [f32],
        codestream: &[u8],
        storage: &DecompositionStorage,
        component_info: &ComponentInfo,
        header: &Header,
        max_layers: Option<u8>,
        low_row: &mut Vec<f32>,
        high_row: &mut Vec<f32>,
        bit_plane_ctx: &mut BitPlaneDecodeContext,
        bit_plane_buffers: &mut BitPlaneDecodeBuffers,
        cancel: Cancel<'_>,
    ) -> Result<()> {
        let is_low_row = row_idx.is_multiple_of(2) == self.low_first_y;
        let v = row_idx / 2;
        if is_low_row {
            let abs_y = self.hl_y0 + v as u32;
            if abs_y >= self.hl_strip_y1 || abs_y < self.hl_strip_y0 {
                let next_grid = (abs_y / self.hl_cb_height + 1).saturating_mul(self.hl_cb_height);
                let abs_y_end = next_grid.min(self.hl_y1);
                decode_sub_band_strip(
                    codestream,
                    self.sub_bands[0],
                    self.resolution,
                    component_info,
                    bit_plane_ctx,
                    bit_plane_buffers,
                    storage,
                    header,
                    max_layers,
                    abs_y,
                    abs_y_end,
                    &mut self.hl_strip,
                    cancel,
                )?;
                self.hl_strip_y0 = abs_y;
                self.hl_strip_y1 = abs_y_end;
            }
            let ll_src = self
                .ll_plane
                .get(v * self.num_low_x..)
                .and_then(|s| s.get(..self.num_low_x))
                .ok_or(DecodeError::Malformed)?;
            let hl_v = (abs_y - self.hl_strip_y0) as usize;
            let hl_src = self
                .hl_strip
                .get(hl_v * self.num_high_x..)
                .and_then(|s| s.get(..self.num_high_x))
                .ok_or(DecodeError::Malformed)?;
            let mut filter = HorizontalFilter::new(
                self.transform,
                self.low_first_x,
                self.num_low_x,
                self.num_high_x,
                low_row,
                high_row,
            )?;
            filter.filter_single_row(simd, ll_src, hl_src, dst)
        } else {
            let abs_y = self.lh_y0 + v as u32;
            if abs_y >= self.lh_strip_y1 || abs_y < self.lh_strip_y0 {
                let next_grid = (abs_y / self.lh_cb_height + 1).saturating_mul(self.lh_cb_height);
                let abs_y_end = next_grid.min(self.lh_y1);
                decode_sub_band_strip(
                    codestream,
                    self.sub_bands[1],
                    self.resolution,
                    component_info,
                    bit_plane_ctx,
                    bit_plane_buffers,
                    storage,
                    header,
                    max_layers,
                    abs_y,
                    abs_y_end,
                    &mut self.lh_strip,
                    cancel,
                )?;
                decode_sub_band_strip(
                    codestream,
                    self.sub_bands[2],
                    self.resolution,
                    component_info,
                    bit_plane_ctx,
                    bit_plane_buffers,
                    storage,
                    header,
                    max_layers,
                    abs_y,
                    abs_y_end,
                    &mut self.hh_strip,
                    cancel,
                )?;
                self.lh_strip_y0 = abs_y;
                self.lh_strip_y1 = abs_y_end;
            }
            let lh_v = (abs_y - self.lh_strip_y0) as usize;
            let lh_src = self
                .lh_strip
                .get(lh_v * self.num_low_x..)
                .and_then(|s| s.get(..self.num_low_x))
                .ok_or(DecodeError::Malformed)?;
            let hh_src = self
                .hh_strip
                .get(lh_v * self.num_high_x..)
                .and_then(|s| s.get(..self.num_high_x))
                .ok_or(DecodeError::Malformed)?;
            let mut filter = HorizontalFilter::new(
                self.transform,
                self.low_first_x,
                self.num_low_x,
                self.num_high_x,
                low_row,
                high_row,
            )?;
            filter.filter_single_row(simd, lh_src, hh_src, dst)
        }
    }

    #[inline(always)]
    fn load_into_ring<S: Simd, const RING: usize>(
        &mut self,
        simd: S,
        ring: &mut [f32],
        row_idx: usize,
        codestream: &[u8],
        storage: &DecompositionStorage,
        component_info: &ComponentInfo,
        header: &Header,
        max_layers: Option<u8>,
        low_row: &mut Vec<f32>,
        high_row: &mut Vec<f32>,
        bit_plane_ctx: &mut BitPlaneDecodeContext,
        bit_plane_buffers: &mut BitPlaneDecodeBuffers,
        cancel: Cancel<'_>,
    ) -> Result<()> {
        let width = self.rect.width() as usize;
        let slot = row_idx & (RING - 1);
        let dst = ring
            .get_mut(slot * width..)
            .and_then(|s| s.get_mut(..width))
            .ok_or(DecodeError::Malformed)?;
        self.load_row(
            simd,
            row_idx,
            dst,
            codestream,
            storage,
            component_info,
            header,
            max_layers,
            low_row,
            high_row,
            bit_plane_ctx,
            bit_plane_buffers,
            cancel,
        )
    }

    #[inline(always)]
    fn advance_to<S: Simd>(
        &mut self,
        simd: S,
        y: usize,
        codestream: &[u8],
        storage: &DecompositionStorage,
        component_info: &ComponentInfo,
        header: &Header,
        max_layers: Option<u8>,
        low_row: &mut Vec<f32>,
        high_row: &mut Vec<f32>,
        bit_plane_ctx: &mut BitPlaneDecodeContext,
        bit_plane_buffers: &mut BitPlaneDecodeBuffers,
        cancel: Cancel<'_>,
    ) -> Result<()> {
        let width = self.rect.width() as usize;
        let height = self.rect.height() as usize;

        let ready = match &self.window {
            WindowState::Small { computed, .. } => *computed,
            WindowState::Ring53r { ready_until, .. } | WindowState::Ring97i { ready_until, .. } => {
                y < *ready_until
            }
        };
        if ready {
            return Ok(());
        }

        let mut window = mem::replace(
            &mut self.window,
            WindowState::Small {
                buf: Vec::new(),
                computed: false,
            },
        );

        let result = match &mut window {
            WindowState::Small { buf, computed } => {
                let len = width.checked_mul(height).ok_or(DecodeError::TooLarge)?;
                buf.resize(len, 0.0);
                for r in 0..height {
                    let dst = buf
                        .get_mut(r * width..)
                        .and_then(|s| s.get_mut(..width))
                        .ok_or(DecodeError::Malformed)?;
                    self.load_row(
                        simd,
                        r,
                        dst,
                        codestream,
                        storage,
                        component_info,
                        header,
                        max_layers,
                        low_row,
                        high_row,
                        bit_plane_ctx,
                        bit_plane_buffers,
                        cancel,
                    )?;
                }
                filter_vertical(simd, buf, width, self.rect, self.transform, cancel)?;
                *computed = true;
                Ok(())
            }
            WindowState::Ring53r {
                ring,
                next_step,
                ready_until,
            } => {
                let lift_low_simd = |s: f32x8<S>, left: f32x8<S>, right: f32x8<S>| {
                    s - ((left + right + 2.0) * 0.25).floor()
                };
                let lift_low_scalar = |s: f32, left: f32, right: f32| {
                    s - math::mul_add(left + right, 0.25, 0.5).floor()
                };
                let lift_high_simd = |s: f32x8<S>, left: f32x8<S>, right: f32x8<S>| {
                    s + ((left + right) * 0.5).floor()
                };
                let lift_high_scalar =
                    |s: f32, left: f32, right: f32| s + ((left + right) * 0.5).floor();

                while y >= *ready_until {
                    if self.low_first_y {
                        if *next_step == 0 {
                            self.load_into_ring::<S, 4>(
                                simd,
                                ring,
                                0,
                                codestream,
                                storage,
                                component_info,
                                header,
                                max_layers,
                                low_row,
                                high_row,
                                bit_plane_ctx,
                                bit_plane_buffers,
                                cancel,
                            )?;
                            self.load_into_ring::<S, 4>(
                                simd,
                                ring,
                                1,
                                codestream,
                                storage,
                                component_info,
                                header,
                                max_layers,
                                low_row,
                                high_row,
                                bit_plane_ctx,
                                bit_plane_buffers,
                                cancel,
                            )?;
                            lift_ring_row::<S, 4>(
                                simd,
                                ring,
                                width,
                                0,
                                1,
                                1,
                                lift_low_simd,
                                lift_low_scalar,
                            )?;
                            *next_step = 2;
                        } else if *next_step < height {
                            let even = *next_step;
                            self.load_into_ring::<S, 4>(
                                simd,
                                ring,
                                even,
                                codestream,
                                storage,
                                component_info,
                                header,
                                max_layers,
                                low_row,
                                high_row,
                                bit_plane_ctx,
                                bit_plane_buffers,
                                cancel,
                            )?;
                            if even + 1 < height {
                                self.load_into_ring::<S, 4>(
                                    simd,
                                    ring,
                                    even + 1,
                                    codestream,
                                    storage,
                                    component_info,
                                    header,
                                    max_layers,
                                    low_row,
                                    high_row,
                                    bit_plane_ctx,
                                    bit_plane_buffers,
                                    cancel,
                                )?;
                            }
                            let below = if even + 1 < height {
                                even + 1
                            } else {
                                even - 1
                            };
                            lift_ring_row::<S, 4>(
                                simd,
                                ring,
                                width,
                                even,
                                even - 1,
                                below,
                                lift_low_simd,
                                lift_low_scalar,
                            )?;
                            lift_ring_row::<S, 4>(
                                simd,
                                ring,
                                width,
                                even - 1,
                                even - 2,
                                even,
                                lift_high_simd,
                                lift_high_scalar,
                            )?;
                            *next_step += 2;
                            *ready_until = even;
                        } else {
                            if height.is_multiple_of(2) {
                                let last_odd = height - 1;
                                lift_ring_row::<S, 4>(
                                    simd,
                                    ring,
                                    width,
                                    last_odd,
                                    last_odd - 1,
                                    last_odd - 1,
                                    lift_high_simd,
                                    lift_high_scalar,
                                )?;
                            }
                            *ready_until = height;
                        }
                    } else if *next_step == 0 {
                        for r in 0..3 {
                            self.load_into_ring::<S, 4>(
                                simd,
                                ring,
                                r,
                                codestream,
                                storage,
                                component_info,
                                header,
                                max_layers,
                                low_row,
                                high_row,
                                bit_plane_ctx,
                                bit_plane_buffers,
                                cancel,
                            )?;
                        }
                        lift_ring_row::<S, 4>(
                            simd,
                            ring,
                            width,
                            1,
                            0,
                            2,
                            lift_low_simd,
                            lift_low_scalar,
                        )?;
                        lift_ring_row::<S, 4>(
                            simd,
                            ring,
                            width,
                            0,
                            1,
                            1,
                            lift_high_simd,
                            lift_high_scalar,
                        )?;
                        *next_step = 3;
                        *ready_until = 1;
                    } else if *next_step < height {
                        let odd = *next_step;
                        self.load_into_ring::<S, 4>(
                            simd,
                            ring,
                            odd,
                            codestream,
                            storage,
                            component_info,
                            header,
                            max_layers,
                            low_row,
                            high_row,
                            bit_plane_ctx,
                            bit_plane_buffers,
                            cancel,
                        )?;
                        if odd + 1 < height {
                            self.load_into_ring::<S, 4>(
                                simd,
                                ring,
                                odd + 1,
                                codestream,
                                storage,
                                component_info,
                                header,
                                max_layers,
                                low_row,
                                high_row,
                                bit_plane_ctx,
                                bit_plane_buffers,
                                cancel,
                            )?;
                        }
                        let below = if odd + 1 < height { odd + 1 } else { odd - 1 };
                        lift_ring_row::<S, 4>(
                            simd,
                            ring,
                            width,
                            odd,
                            odd - 1,
                            below,
                            lift_low_simd,
                            lift_low_scalar,
                        )?;
                        lift_ring_row::<S, 4>(
                            simd,
                            ring,
                            width,
                            odd - 1,
                            odd - 2,
                            odd,
                            lift_high_simd,
                            lift_high_scalar,
                        )?;
                        *next_step += 2;
                        *ready_until = odd;
                    } else {
                        if !height.is_multiple_of(2) {
                            let last_even = height - 1;
                            lift_ring_row::<S, 4>(
                                simd,
                                ring,
                                width,
                                last_even,
                                last_even - 1,
                                last_even - 1,
                                lift_high_simd,
                                lift_high_scalar,
                            )?;
                        }
                        *ready_until = height;
                    }
                }
                Ok(())
            }
            WindowState::Ring97i {
                ring,
                next_step,
                ready_until,
            } => {
                let neg_alpha = f32x8::splat(simd, NEG_ALPHA);
                let neg_beta = f32x8::splat(simd, NEG_BETA);
                let neg_gamma = f32x8::splat(simd, NEG_GAMMA);
                let neg_delta = f32x8::splat(simd, NEG_DELTA);

                let delta_simd =
                    |s: f32x8<S>, l: f32x8<S>, r: f32x8<S>| (l + r).mul_add(neg_delta, s);
                let delta_scalar = |s: f32, l: f32, r: f32| math::mul_add(l + r, NEG_DELTA, s);
                let gamma_simd =
                    |s: f32x8<S>, l: f32x8<S>, r: f32x8<S>| (l + r).mul_add(neg_gamma, s);
                let gamma_scalar = |s: f32, l: f32, r: f32| math::mul_add(l + r, NEG_GAMMA, s);
                let beta_simd =
                    |s: f32x8<S>, l: f32x8<S>, r: f32x8<S>| (l + r).mul_add(neg_beta, s);
                let beta_scalar = |s: f32, l: f32, r: f32| math::mul_add(l + r, NEG_BETA, s);
                let alpha_simd =
                    |s: f32x8<S>, l: f32x8<S>, r: f32x8<S>| (l + r).mul_add(neg_alpha, s);
                let alpha_scalar = |s: f32, l: f32, r: f32| math::mul_add(l + r, NEG_ALPHA, s);

                while y >= *ready_until {
                    if self.low_first_y {
                        if *next_step < height {
                            let even = *next_step;
                            self.load_into_ring::<S, 8>(
                                simd,
                                ring,
                                even,
                                codestream,
                                storage,
                                component_info,
                                header,
                                max_layers,
                                low_row,
                                high_row,
                                bit_plane_ctx,
                                bit_plane_buffers,
                                cancel,
                            )?;
                            scale_ring_row::<S, 8>(simd, ring, width, even, KAPPA)?;
                            if even + 1 < height {
                                self.load_into_ring::<S, 8>(
                                    simd,
                                    ring,
                                    even + 1,
                                    codestream,
                                    storage,
                                    component_info,
                                    header,
                                    max_layers,
                                    low_row,
                                    high_row,
                                    bit_plane_ctx,
                                    bit_plane_buffers,
                                    cancel,
                                )?;
                                scale_ring_row::<S, 8>(simd, ring, width, even + 1, INV_KAPPA)?;
                            }

                            let above = if even == 0 { 1 } else { even - 1 };
                            let below = if even + 1 < height {
                                even + 1
                            } else {
                                even - 1
                            };
                            lift_ring_row::<S, 8>(
                                simd,
                                ring,
                                width,
                                even,
                                above,
                                below,
                                delta_simd,
                                delta_scalar,
                            )?;

                            if even >= 2 {
                                lift_ring_row::<S, 8>(
                                    simd,
                                    ring,
                                    width,
                                    even - 1,
                                    even - 2,
                                    even,
                                    gamma_simd,
                                    gamma_scalar,
                                )?;
                                let beta_row = even - 2;
                                let beta_above = if beta_row == 0 { 1 } else { beta_row - 1 };
                                lift_ring_row::<S, 8>(
                                    simd,
                                    ring,
                                    width,
                                    beta_row,
                                    beta_above,
                                    beta_row + 1,
                                    beta_simd,
                                    beta_scalar,
                                )?;
                            }

                            if even >= 4 {
                                lift_ring_row::<S, 8>(
                                    simd,
                                    ring,
                                    width,
                                    even - 3,
                                    even - 4,
                                    even - 2,
                                    alpha_simd,
                                    alpha_scalar,
                                )?;
                                *ready_until = even - 2;
                            }

                            *next_step += 2;
                        } else {
                            let last_even = (height - 1) & !1;
                            if height.is_multiple_of(2) {
                                let last_odd = height - 1;
                                lift_ring_row::<S, 8>(
                                    simd,
                                    ring,
                                    width,
                                    last_odd,
                                    last_even,
                                    last_even,
                                    gamma_simd,
                                    gamma_scalar,
                                )?;
                                lift_ring_row::<S, 8>(
                                    simd,
                                    ring,
                                    width,
                                    last_even,
                                    last_even - 1,
                                    last_odd,
                                    beta_simd,
                                    beta_scalar,
                                )?;
                                lift_ring_row::<S, 8>(
                                    simd,
                                    ring,
                                    width,
                                    last_even - 1,
                                    last_even - 2,
                                    last_even,
                                    alpha_simd,
                                    alpha_scalar,
                                )?;
                                lift_ring_row::<S, 8>(
                                    simd,
                                    ring,
                                    width,
                                    last_odd,
                                    last_even,
                                    last_even,
                                    alpha_simd,
                                    alpha_scalar,
                                )?;
                            } else {
                                lift_ring_row::<S, 8>(
                                    simd,
                                    ring,
                                    width,
                                    last_even,
                                    last_even - 1,
                                    last_even - 1,
                                    beta_simd,
                                    beta_scalar,
                                )?;
                                lift_ring_row::<S, 8>(
                                    simd,
                                    ring,
                                    width,
                                    last_even - 1,
                                    last_even - 2,
                                    last_even,
                                    alpha_simd,
                                    alpha_scalar,
                                )?;
                            }
                            *ready_until = height;
                        }
                    } else if *next_step == 0 {
                        for r in 0..3 {
                            self.load_into_ring::<S, 8>(
                                simd,
                                ring,
                                r,
                                codestream,
                                storage,
                                component_info,
                                header,
                                max_layers,
                                low_row,
                                high_row,
                                bit_plane_ctx,
                                bit_plane_buffers,
                                cancel,
                            )?;
                        }
                        scale_ring_row::<S, 8>(simd, ring, width, 0, INV_KAPPA)?;
                        scale_ring_row::<S, 8>(simd, ring, width, 1, KAPPA)?;
                        scale_ring_row::<S, 8>(simd, ring, width, 2, INV_KAPPA)?;
                        lift_ring_row::<S, 8>(
                            simd,
                            ring,
                            width,
                            1,
                            0,
                            2,
                            delta_simd,
                            delta_scalar,
                        )?;
                        lift_ring_row::<S, 8>(
                            simd,
                            ring,
                            width,
                            0,
                            1,
                            1,
                            gamma_simd,
                            gamma_scalar,
                        )?;
                        *next_step = 3;
                    } else if *next_step < height {
                        let odd = *next_step;
                        self.load_into_ring::<S, 8>(
                            simd,
                            ring,
                            odd,
                            codestream,
                            storage,
                            component_info,
                            header,
                            max_layers,
                            low_row,
                            high_row,
                            bit_plane_ctx,
                            bit_plane_buffers,
                            cancel,
                        )?;
                        scale_ring_row::<S, 8>(simd, ring, width, odd, KAPPA)?;
                        if odd + 1 < height {
                            self.load_into_ring::<S, 8>(
                                simd,
                                ring,
                                odd + 1,
                                codestream,
                                storage,
                                component_info,
                                header,
                                max_layers,
                                low_row,
                                high_row,
                                bit_plane_ctx,
                                bit_plane_buffers,
                                cancel,
                            )?;
                            scale_ring_row::<S, 8>(simd, ring, width, odd + 1, INV_KAPPA)?;
                        }
                        let below = if odd + 1 < height { odd + 1 } else { odd - 1 };
                        lift_ring_row::<S, 8>(
                            simd,
                            ring,
                            width,
                            odd,
                            odd - 1,
                            below,
                            delta_simd,
                            delta_scalar,
                        )?;
                        lift_ring_row::<S, 8>(
                            simd,
                            ring,
                            width,
                            odd - 1,
                            odd - 2,
                            odd,
                            gamma_simd,
                            gamma_scalar,
                        )?;
                        lift_ring_row::<S, 8>(
                            simd,
                            ring,
                            width,
                            odd - 2,
                            odd - 3,
                            odd - 1,
                            beta_simd,
                            beta_scalar,
                        )?;
                        let alpha_row = odd - 3;
                        let alpha_above = if alpha_row == 0 { 1 } else { alpha_row - 1 };
                        lift_ring_row::<S, 8>(
                            simd,
                            ring,
                            width,
                            alpha_row,
                            alpha_above,
                            alpha_row + 1,
                            alpha_simd,
                            alpha_scalar,
                        )?;
                        *ready_until = odd - 2;
                        *next_step += 2;
                    } else {
                        if !height.is_multiple_of(2) {
                            let last_even = height - 1;
                            let last_odd = height - 2;
                            lift_ring_row::<S, 8>(
                                simd,
                                ring,
                                width,
                                last_even,
                                last_odd,
                                last_odd,
                                gamma_simd,
                                gamma_scalar,
                            )?;
                            lift_ring_row::<S, 8>(
                                simd,
                                ring,
                                width,
                                last_odd,
                                last_odd - 1,
                                last_even,
                                beta_simd,
                                beta_scalar,
                            )?;
                            lift_ring_row::<S, 8>(
                                simd,
                                ring,
                                width,
                                last_odd - 1,
                                last_odd - 2,
                                last_odd,
                                alpha_simd,
                                alpha_scalar,
                            )?;
                            lift_ring_row::<S, 8>(
                                simd,
                                ring,
                                width,
                                last_even,
                                last_odd,
                                last_odd,
                                alpha_simd,
                                alpha_scalar,
                            )?;
                        } else {
                            let last_odd = height - 1;
                            lift_ring_row::<S, 8>(
                                simd,
                                ring,
                                width,
                                last_odd,
                                last_odd - 1,
                                last_odd - 1,
                                beta_simd,
                                beta_scalar,
                            )?;
                            lift_ring_row::<S, 8>(
                                simd,
                                ring,
                                width,
                                last_odd - 1,
                                last_odd - 2,
                                last_odd,
                                alpha_simd,
                                alpha_scalar,
                            )?;
                        }
                        *ready_until = height;
                    }
                }
                Ok(())
            }
        };

        self.window = window;
        result
    }

    #[inline(always)]
    fn row(&self, y: usize) -> Result<&[f32]> {
        let width = self.rect.width() as usize;
        match &self.window {
            WindowState::Small { buf, .. } => buf
                .get(y * width..)
                .and_then(|s| s.get(..width))
                .ok_or(DecodeError::Malformed),
            WindowState::Ring53r { ring, .. } => {
                let slot = y & 3;
                ring.get(slot * width..)
                    .and_then(|s| s.get(..width))
                    .ok_or(DecodeError::Malformed)
            }
            WindowState::Ring97i { ring, .. } => {
                let slot = y & 7;
                ring.get(slot * width..)
                    .and_then(|s| s.get(..width))
                    .ok_or(DecodeError::Malformed)
            }
        }
    }
}

struct HorizontalFilter<'a> {
    transform: WaveletTransform,
    low_first: bool,
    num_low: usize,
    num_high: usize,
    low: &'a mut [f32],
    high: &'a mut [f32],
}

impl<'a> HorizontalFilter<'a> {
    fn new(
        transform: WaveletTransform,
        low_first: bool,
        num_low: usize,
        num_high: usize,
        low: &'a mut Vec<f32>,
        high: &'a mut Vec<f32>,
    ) -> Result<Self> {
        let low_len = num_low + 2;
        if low.len() < low_len {
            low.resize(low_len, 0.0);
        }
        let high_len = num_high + 2;
        if high.len() < high_len {
            high.resize(high_len, 0.0);
        }
        let low = low.get_mut(..low_len).ok_or(DecodeError::Malformed)?;
        let high = high.get_mut(..high_len).ok_or(DecodeError::Malformed)?;
        Ok(Self {
            transform,
            low_first,
            num_low,
            num_high,
            low,
            high,
        })
    }

    #[inline(always)]
    fn filter_single_row<S: Simd>(
        &mut self,
        simd: S,
        low_src: &[f32],
        high_src: &[f32],
        row: &mut [f32],
    ) -> Result<()> {
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

        filter_1d(simd, self.transform, self)?;

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

    #[inline(always)]
    fn filter_rows<S: Simd>(
        &mut self,
        simd: S,
        low_band: &[f32],
        high_band: &[f32],
        coefficients: &mut [f32],
        width: usize,
        dst_stride: usize,
        start_row: usize,
        strip_h: usize,
        cancel: Cancel<'_>,
    ) -> Result<()> {
        for v in 0..strip_h {
            cancel.check()?;

            let row = coefficients
                .get_mut((start_row + v * 2) * dst_stride..)
                .and_then(|s| s.get_mut(..width))
                .ok_or(DecodeError::Malformed)?;

            let low_src = low_band
                .get(v * self.num_low..)
                .and_then(|s| s.get(..self.num_low))
                .ok_or(DecodeError::Malformed)?;
            let high_src = high_band
                .get(v * self.num_high..)
                .and_then(|s| s.get(..self.num_high))
                .ok_or(DecodeError::Malformed)?;

            self.filter_single_row(simd, low_src, high_src, row)?;
        }

        Ok(())
    }
}

impl<S: Simd> Lifting<S> for HorizontalFilter<'_> {
    #[inline(always)]
    fn num_samples(&self) -> usize {
        self.num_low + self.num_high
    }

    #[inline(always)]
    fn scale_low(&mut self, simd: S, factor: f32) -> Result<()> {
        if let Some(dst) = self.low.get_mut(1..=self.num_low) {
            scale(simd, dst, factor);
        }

        Ok(())
    }

    #[inline(always)]
    fn scale_high(&mut self, simd: S, factor: f32) -> Result<()> {
        if let Some(dst) = self.high.get_mut(1..=self.num_high) {
            scale(simd, dst, factor);
        }

        Ok(())
    }

    #[inline(always)]
    fn lift_low(
        &mut self,
        simd: S,
        f_simd: impl Fn(f32x8<S>, f32x8<S>, f32x8<S>) -> f32x8<S>,
        f_scalar: impl Fn(f32, f32, f32) -> f32,
    ) -> Result<()> {
        let offset = usize::from(self.low_first);
        lift_band(simd, self.low, self.high, offset, f_simd, f_scalar);

        Ok(())
    }

    #[inline(always)]
    fn lift_high(
        &mut self,
        simd: S,
        f_simd: impl Fn(f32x8<S>, f32x8<S>, f32x8<S>) -> f32x8<S>,
        f_scalar: impl Fn(f32, f32, f32) -> f32,
    ) -> Result<()> {
        let offset = usize::from(!self.low_first);
        lift_band(simd, self.high, self.low, offset, f_simd, f_scalar);

        Ok(())
    }
}

struct VerticalFilter<'a> {
    cancel: Cancel<'a>,
    coefficients: &'a mut [f32],
    width: usize,
    stride: usize,
    height: usize,
    low_first: bool,
}

impl VerticalFilter<'_> {
    #[inline(always)]
    fn scale_rows<S: Simd>(&mut self, simd: S, first_row: usize, factor: f32) -> Result<()> {
        for row in (first_row..self.height).step_by(2) {
            self.cancel.check()?;
            scale_row(
                simd,
                self.coefficients,
                self.width,
                self.stride,
                row,
                factor,
            )?;
        }

        Ok(())
    }

    #[inline(always)]
    fn lift_rows<S: Simd>(
        &mut self,
        simd: S,
        first_row: usize,
        f_simd: impl Fn(f32x8<S>, f32x8<S>, f32x8<S>) -> f32x8<S>,
        f_scalar: impl Fn(f32, f32, f32) -> f32,
    ) -> Result<()> {
        let width = self.width;
        let stride = self.stride;
        let height = self.height;

        for row in (first_row..height).step_by(2) {
            self.cancel.check()?;

            let above = if row == 0 { 1 } else { row - 1 };
            let below = if row == height - 1 {
                height - 2
            } else {
                row + 1
            };

            lift_row(
                simd,
                self.coefficients,
                width,
                stride,
                row,
                above,
                below,
                &f_simd,
                &f_scalar,
            )?;
        }

        Ok(())
    }
}

impl<S: Simd> Lifting<S> for VerticalFilter<'_> {
    #[inline(always)]
    fn num_samples(&self) -> usize {
        self.height
    }

    #[inline(always)]
    fn scale_low(&mut self, simd: S, factor: f32) -> Result<()> {
        self.scale_rows(simd, usize::from(!self.low_first), factor)
    }

    #[inline(always)]
    fn scale_high(&mut self, simd: S, factor: f32) -> Result<()> {
        self.scale_rows(simd, usize::from(self.low_first), factor)
    }

    #[inline(always)]
    fn lift_low(
        &mut self,
        simd: S,
        f_simd: impl Fn(f32x8<S>, f32x8<S>, f32x8<S>) -> f32x8<S>,
        f_scalar: impl Fn(f32, f32, f32) -> f32,
    ) -> Result<()> {
        self.lift_rows(simd, usize::from(!self.low_first), f_simd, f_scalar)
    }

    #[inline(always)]
    fn lift_high(
        &mut self,
        simd: S,
        f_simd: impl Fn(f32x8<S>, f32x8<S>, f32x8<S>) -> f32x8<S>,
        f_scalar: impl Fn(f32, f32, f32) -> f32,
    ) -> Result<()> {
        self.lift_rows(simd, usize::from(self.low_first), f_simd, f_scalar)
    }
}

trait Lifting<S: Simd> {
    fn num_samples(&self) -> usize;

    fn scale_low(&mut self, simd: S, factor: f32) -> Result<()>;

    fn scale_high(&mut self, simd: S, factor: f32) -> Result<()>;

    fn lift_low(
        &mut self,
        simd: S,
        f_simd: impl Fn(f32x8<S>, f32x8<S>, f32x8<S>) -> f32x8<S>,
        f_scalar: impl Fn(f32, f32, f32) -> f32,
    ) -> Result<()>;

    fn lift_high(
        &mut self,
        simd: S,
        f_simd: impl Fn(f32x8<S>, f32x8<S>, f32x8<S>) -> f32x8<S>,
        f_scalar: impl Fn(f32, f32, f32) -> f32,
    ) -> Result<()>;
}

pub(crate) fn apply_tile(
    codestream: &[u8],
    storage: &DecompositionStorage,
    tile_rect: IntRect,
    component_infos: &[ComponentInfo],
    tile_mct: Option<WaveletTransform>,
    bit_plane_ctx: &mut BitPlaneDecodeContext,
    bit_plane_buffers: &mut BitPlaneDecodeBuffers,
    header: &Header,
    params: DecodeParams,
    out: &mut [u8],
    cancel: Cancel<'_>,
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
    let mut scratch = IntermediateScratch::default();
    let mut stages = Vec::with_capacity(num_components);

    for (idx, component_info) in component_infos.iter().enumerate() {
        cancel.check()?;
        let stage = prepare_component_stage(
            codestream,
            storage,
            &mut scratch,
            bit_plane_ctx,
            bit_plane_buffers,
            idx,
            component_info,
            header,
            params,
            cancel,
        )?;
        stages.push(stage);
    }

    let mut low_row = scratch.low_row;
    let mut high_row = scratch.high_row;
    drop(scratch.buffer);
    drop(scratch.aux_buffer);
    drop(scratch.strip_a);
    drop(scratch.strip_b);

    dispatch!(Level::new(), simd => {
        for y in 0..height {
            cancel.check()?;
            for (stage, component_info) in stages.iter_mut().zip(component_infos) {
                stage.advance_to(
                    simd,
                    y,
                    codestream,
                    storage,
                    component_info,
                    header,
                    params.max_layers,
                    &mut low_row,
                    &mut high_row,
                    bit_plane_ctx,
                    bit_plane_buffers,
                    cancel,
                )?;
            }

            let row_start = ((y0 + y) * image_width + x0) * num_components;
            let row_len = width * num_components;
            let out_row = out
                .get_mut(row_start..)
                .and_then(|s| s.get_mut(..row_len))
                .ok_or(DecodeError::Malformed)?;

            match stages.as_slice() {
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
        }
        Ok(())
    })
}

fn prepare_component_stage(
    codestream: &[u8],
    storage: &DecompositionStorage,
    scratch: &mut IntermediateScratch,
    bit_plane_ctx: &mut BitPlaneDecodeContext,
    bit_plane_buffers: &mut BitPlaneDecodeBuffers,
    component_idx: usize,
    component_info: &ComponentInfo,
    header: &Header,
    params: DecodeParams,
    cancel: Cancel<'_>,
) -> Result<ComponentStage> {
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
            .saturating_sub(params.skipped_resolution_levels)
            .saturating_sub(1),
    );
    let decompositions = decompositions.get(..keep).ok_or(DecodeError::Malformed)?;
    let ll_sub_band = storage
        .sub_bands
        .get(tile_decompositions.first_ll_sub_band)
        .ok_or(DecodeError::Malformed)?;

    let Some((last, intermediate)) = decompositions.split_last() else {
        return Ok(ComponentStage::Direct(DirectStage {
            sub_band_idx: tile_decompositions.first_ll_sub_band,
            y0: ll_sub_band.rect.y0,
            y1: ll_sub_band.rect.y1,
            cb_height: ll_sub_band.cb_height,
            width: ll_sub_band.rect.width() as usize,
            strip_y0: 0,
            strip_y1: 0,
            strip: Vec::new(),
        }));
    };

    let buffer_size = |decomposition: &Decomposition| {
        decomposition.rect.width() as usize * decomposition.rect.height() as usize
    };

    let mut ll_plane = Vec::new();
    if let Some((first, middle)) = intermediate.split_first() {
        let max_half_width = (last.rect.width() as usize).div_ceil(2) + 2;
        if scratch.low_row.capacity() < max_half_width {
            scratch.low_row.clear();
            scratch.low_row.reserve_exact(max_half_width);
        }
        if scratch.high_row.capacity() < max_half_width {
            scratch.high_row.clear();
            scratch.high_row.reserve_exact(max_half_width);
        }

        let mut max_strip_a = 0_usize;
        let mut max_strip_b =
            usize::try_from(ll_sub_band.rect.area()).map_err(|_| DecodeError::TooLarge)?;
        for decomposition in intermediate {
            let [hl_idx, lh_idx, hh_idx] = decomposition.sub_bands;
            let strip_len = |idx: usize| -> Result<usize> {
                let sb = storage.sub_bands.get(idx).ok_or(DecodeError::Malformed)?;
                let h = sb.rect.height().min(sb.cb_height) as usize;
                (sb.rect.width() as usize)
                    .checked_mul(h)
                    .ok_or(DecodeError::TooLarge)
            };
            max_strip_a = max_strip_a.max(strip_len(hl_idx)?).max(strip_len(lh_idx)?);
            max_strip_b = max_strip_b.max(strip_len(hh_idx)?);
        }
        if scratch.strip_a.capacity() < max_strip_a {
            scratch.strip_a.clear();
            scratch.strip_a.reserve_exact(max_strip_a);
        }
        if scratch.strip_b.capacity() < max_strip_b {
            scratch.strip_b.clear();
            scratch.strip_b.reserve_exact(max_strip_b);
        }

        decode_sub_band_strip(
            codestream,
            tile_decompositions.first_ll_sub_band,
            0,
            component_info,
            bit_plane_ctx,
            bit_plane_buffers,
            storage,
            header,
            params.max_layers,
            ll_sub_band.rect.y0,
            ll_sub_band.rect.y1,
            &mut scratch.strip_b,
            cancel,
        )?;

        if middle.is_empty() {
            let len = buffer_size(first);
            cancel.ensure_len(&mut ll_plane, len)?;
            filter_2d_first(
                codestream,
                &mut ll_plane,
                first.rect.width() as usize,
                &mut scratch.low_row,
                &mut scratch.high_row,
                &mut scratch.strip_a,
                &mut scratch.strip_b,
                bit_plane_ctx,
                bit_plane_buffers,
                first,
                1,
                component_info,
                storage,
                header,
                params.max_layers,
                cancel,
            )?;
        } else {
            let (last_mid, earlier_mid) = middle.split_last().ok_or(DecodeError::Malformed)?;
            let mut use_buffer = !earlier_mid.len().is_multiple_of(2);
            let first_len = buffer_size(first);
            let first_width = first.rect.width() as usize;

            if use_buffer {
                cancel.ensure_len(&mut scratch.buffer, first_len)?;
                let dst = scratch
                    .buffer
                    .get_mut(..first_len)
                    .ok_or(DecodeError::Malformed)?;
                filter_2d_first(
                    codestream,
                    dst,
                    first_width,
                    &mut scratch.low_row,
                    &mut scratch.high_row,
                    &mut scratch.strip_a,
                    &mut scratch.strip_b,
                    bit_plane_ctx,
                    bit_plane_buffers,
                    first,
                    1,
                    component_info,
                    storage,
                    header,
                    params.max_layers,
                    cancel,
                )?;
            } else {
                cancel.ensure_len(&mut scratch.aux_buffer, first_len)?;
                let dst = scratch
                    .aux_buffer
                    .get_mut(..first_len)
                    .ok_or(DecodeError::Malformed)?;
                filter_2d_first(
                    codestream,
                    dst,
                    first_width,
                    &mut scratch.low_row,
                    &mut scratch.high_row,
                    &mut scratch.strip_a,
                    &mut scratch.strip_b,
                    bit_plane_ctx,
                    bit_plane_buffers,
                    first,
                    1,
                    component_info,
                    storage,
                    header,
                    params.max_layers,
                    cancel,
                )?;
            }

            for (idx, decomposition) in earlier_mid.iter().enumerate() {
                let resolution = (idx + 2) as u8;
                use_buffer = !use_buffer;
                let len = buffer_size(decomposition);
                let width = decomposition.rect.width() as usize;
                if use_buffer {
                    cancel.ensure_len(&mut scratch.buffer, len)?;
                    let dst = scratch
                        .buffer
                        .get_mut(..len)
                        .ok_or(DecodeError::Malformed)?;
                    filter_2d(
                        codestream,
                        &scratch.aux_buffer,
                        dst,
                        width,
                        &mut scratch.low_row,
                        &mut scratch.high_row,
                        &mut scratch.strip_a,
                        &mut scratch.strip_b,
                        bit_plane_ctx,
                        bit_plane_buffers,
                        decomposition,
                        resolution,
                        component_info,
                        storage,
                        header,
                        params.max_layers,
                        cancel,
                    )?;
                } else {
                    cancel.ensure_len(&mut scratch.aux_buffer, len)?;
                    let dst = scratch
                        .aux_buffer
                        .get_mut(..len)
                        .ok_or(DecodeError::Malformed)?;
                    filter_2d(
                        codestream,
                        &scratch.buffer,
                        dst,
                        width,
                        &mut scratch.low_row,
                        &mut scratch.high_row,
                        &mut scratch.strip_a,
                        &mut scratch.strip_b,
                        bit_plane_ctx,
                        bit_plane_buffers,
                        decomposition,
                        resolution,
                        component_info,
                        storage,
                        header,
                        params.max_layers,
                        cancel,
                    )?;
                }
            }

            let last_mid_res = (earlier_mid.len() + 2) as u8;
            let last_mid_len = buffer_size(last_mid);
            cancel.ensure_len(&mut ll_plane, last_mid_len)?;
            let src = if use_buffer {
                &scratch.buffer
            } else {
                &scratch.aux_buffer
            };
            filter_2d(
                codestream,
                src,
                &mut ll_plane,
                last_mid.rect.width() as usize,
                &mut scratch.low_row,
                &mut scratch.high_row,
                &mut scratch.strip_a,
                &mut scratch.strip_b,
                bit_plane_ctx,
                bit_plane_buffers,
                last_mid,
                last_mid_res,
                component_info,
                storage,
                header,
                params.max_layers,
                cancel,
            )?;
        }
    } else {
        decode_sub_band_strip(
            codestream,
            tile_decompositions.first_ll_sub_band,
            0,
            component_info,
            bit_plane_ctx,
            bit_plane_buffers,
            storage,
            header,
            params.max_layers,
            ll_sub_band.rect.y0,
            ll_sub_band.rect.y1,
            &mut ll_plane,
            cancel,
        )?;
    }

    let IntRect {
        x0: u0,
        x1: u1,
        y0: v0,
        ..
    } = last.rect;
    let width = last.rect.width() as usize;
    let height = last.rect.height() as usize;
    let transform = component_info.wavelet_transform();
    let hl_sb = storage
        .sub_bands
        .get(last.sub_bands[0])
        .ok_or(DecodeError::Malformed)?;
    let lh_sb = storage
        .sub_bands
        .get(last.sub_bands[1])
        .ok_or(DecodeError::Malformed)?;

    let window = match transform {
        WaveletTransform::Reversible53 if height >= 4 => WindowState::Ring53r {
            ring: vec![0.0; 4 * width],
            next_step: 0,
            ready_until: 0,
        },
        WaveletTransform::Irreversible97 if height >= 8 => WindowState::Ring97i {
            ring: vec![0.0; 8 * width],
            next_step: 0,
            ready_until: 0,
        },
        _ => WindowState::Small {
            buf: Vec::new(),
            computed: false,
        },
    };

    Ok(ComponentStage::LastDwt(LastStage {
        ll_plane,
        rect: last.rect,
        resolution: decompositions.len() as u8,
        sub_bands: last.sub_bands,
        transform,
        low_first_x: u0.is_multiple_of(2),
        low_first_y: v0.is_multiple_of(2),
        num_low_x: (u1.div_ceil(2) - u0.div_ceil(2)) as usize,
        num_high_x: (u1 / 2 - u0 / 2) as usize,
        hl_y0: hl_sb.rect.y0,
        hl_y1: hl_sb.rect.y1,
        hl_cb_height: hl_sb.cb_height,
        hl_strip_y0: 0,
        hl_strip_y1: 0,
        hl_strip: Vec::new(),
        lh_y0: lh_sb.rect.y0,
        lh_y1: lh_sb.rect.y1,
        lh_cb_height: lh_sb.cb_height,
        lh_strip_y0: 0,
        lh_strip_y1: 0,
        lh_strip: Vec::new(),
        hh_strip: Vec::new(),
        window,
    }))
}

fn filter_2d_first(
    codestream: &[u8],
    coefficients: &mut [f32],
    dst_stride: usize,
    low_row: &mut Vec<f32>,
    high_row: &mut Vec<f32>,
    strip_a: &mut Vec<f32>,
    strip_b: &mut Vec<f32>,
    bit_plane_ctx: &mut BitPlaneDecodeContext,
    bit_plane_buffers: &mut BitPlaneDecodeBuffers,
    decomposition: &Decomposition,
    resolution: u8,
    component_info: &ComponentInfo,
    storage: &DecompositionStorage,
    header: &Header,
    max_layers: Option<u8>,
    cancel: Cancel<'_>,
) -> Result<()> {
    let width = decomposition.rect.width() as usize;
    let height = decomposition.rect.height() as usize;
    if width == 0 || height == 0 {
        return Ok(());
    }

    dispatch!(Level::new(), simd => {
        let transform = component_info.wavelet_transform();
        let IntRect {
            x0: u0,
            x1: u1,
            y0: v0,
            ..
        } = decomposition.rect;
        let num_low = (u1.div_ceil(2) - u0.div_ceil(2)) as usize;
        let num_high = (u1 / 2 - u0 / 2) as usize;
        let (first_low_row, first_high_row) = if v0.is_multiple_of(2) { (0, 1) } else { (1, 0) };
        let [hl_idx, lh_idx, hh_idx] = decomposition.sub_bands;

        filter_low_rows_streaming(
            simd,
            codestream,
            strip_b,
            hl_idx,
            resolution,
            component_info,
            bit_plane_ctx,
            bit_plane_buffers,
            storage,
            header,
            max_layers,
            strip_a,
            low_row,
            high_row,
            coefficients,
            width,
            dst_stride,
            first_low_row,
            u0.is_multiple_of(2),
            num_low,
            num_high,
            transform,
            cancel,
        )?;
        filter_high_rows_streaming(
            simd,
            codestream,
            lh_idx,
            hh_idx,
            resolution,
            component_info,
            bit_plane_ctx,
            bit_plane_buffers,
            storage,
            header,
            max_layers,
            strip_a,
            strip_b,
            low_row,
            high_row,
            coefficients,
            width,
            dst_stride,
            first_high_row,
            u0.is_multiple_of(2),
            num_low,
            num_high,
            transform,
            cancel,
        )?;
        filter_vertical(
            simd,
            coefficients,
            dst_stride,
            decomposition.rect,
            transform,
            cancel,
        )
    })
}

fn filter_2d(
    codestream: &[u8],
    ll: &[f32],
    coefficients: &mut [f32],
    dst_stride: usize,
    low_row: &mut Vec<f32>,
    high_row: &mut Vec<f32>,
    strip_a: &mut Vec<f32>,
    strip_b: &mut Vec<f32>,
    bit_plane_ctx: &mut BitPlaneDecodeContext,
    bit_plane_buffers: &mut BitPlaneDecodeBuffers,
    decomposition: &Decomposition,
    resolution: u8,
    component_info: &ComponentInfo,
    storage: &DecompositionStorage,
    header: &Header,
    max_layers: Option<u8>,
    cancel: Cancel<'_>,
) -> Result<()> {
    let width = decomposition.rect.width() as usize;
    let height = decomposition.rect.height() as usize;
    if width == 0 || height == 0 {
        return Ok(());
    }

    dispatch!(Level::new(), simd => {
        let transform = component_info.wavelet_transform();
        let IntRect {
            x0: u0,
            x1: u1,
            y0: v0,
            ..
        } = decomposition.rect;
        let num_low = (u1.div_ceil(2) - u0.div_ceil(2)) as usize;
        let num_high = (u1 / 2 - u0 / 2) as usize;
        let (first_low_row, first_high_row) = if v0.is_multiple_of(2) { (0, 1) } else { (1, 0) };
        let [hl_idx, lh_idx, hh_idx] = decomposition.sub_bands;

        filter_low_rows_streaming(
            simd,
            codestream,
            ll,
            hl_idx,
            resolution,
            component_info,
            bit_plane_ctx,
            bit_plane_buffers,
            storage,
            header,
            max_layers,
            strip_a,
            low_row,
            high_row,
            coefficients,
            width,
            dst_stride,
            first_low_row,
            u0.is_multiple_of(2),
            num_low,
            num_high,
            transform,
            cancel,
        )?;
        filter_high_rows_streaming(
            simd,
            codestream,
            lh_idx,
            hh_idx,
            resolution,
            component_info,
            bit_plane_ctx,
            bit_plane_buffers,
            storage,
            header,
            max_layers,
            strip_a,
            strip_b,
            low_row,
            high_row,
            coefficients,
            width,
            dst_stride,
            first_high_row,
            u0.is_multiple_of(2),
            num_low,
            num_high,
            transform,
            cancel,
        )?;
        filter_vertical(
            simd,
            coefficients,
            dst_stride,
            decomposition.rect,
            transform,
            cancel,
        )
    })
}

#[inline(always)]
fn filter_low_rows_streaming<S: Simd>(
    simd: S,
    codestream: &[u8],
    ll: &[f32],
    hl_idx: usize,
    resolution: u8,
    component_info: &ComponentInfo,
    bit_plane_ctx: &mut BitPlaneDecodeContext,
    bit_plane_buffers: &mut BitPlaneDecodeBuffers,
    storage: &DecompositionStorage,
    header: &Header,
    max_layers: Option<u8>,
    hl_strip: &mut Vec<f32>,
    low_row: &mut Vec<f32>,
    high_row: &mut Vec<f32>,
    coefficients: &mut [f32],
    width: usize,
    dst_stride: usize,
    first_row: usize,
    low_first: bool,
    num_low: usize,
    num_high: usize,
    transform: WaveletTransform,
    cancel: Cancel<'_>,
) -> Result<()> {
    let hl_sb = storage
        .sub_bands
        .get(hl_idx)
        .ok_or(DecodeError::Malformed)?;
    let y0 = hl_sb.rect.y0;
    let y1 = hl_sb.rect.y1;
    let cb_height = hl_sb.cb_height;
    let mut filter =
        HorizontalFilter::new(transform, low_first, num_low, num_high, low_row, high_row)?;

    let mut abs_y = y0;
    while abs_y < y1 {
        let next_grid = (abs_y / cb_height + 1).saturating_mul(cb_height);
        let abs_y_end = next_grid.min(y1);
        let v_start = (abs_y - y0) as usize;
        let strip_h = (abs_y_end - abs_y) as usize;

        decode_sub_band_strip(
            codestream,
            hl_idx,
            resolution,
            component_info,
            bit_plane_ctx,
            bit_plane_buffers,
            storage,
            header,
            max_layers,
            abs_y,
            abs_y_end,
            hl_strip,
            cancel,
        )?;

        let ll_start = v_start.checked_mul(num_low).ok_or(DecodeError::Malformed)?;
        let ll_len = strip_h.checked_mul(num_low).ok_or(DecodeError::Malformed)?;
        let ll_slice = ll
            .get(ll_start..)
            .and_then(|s| s.get(..ll_len))
            .ok_or(DecodeError::Malformed)?;

        filter.filter_rows(
            simd,
            ll_slice,
            hl_strip,
            coefficients,
            width,
            dst_stride,
            first_row + v_start * 2,
            strip_h,
            cancel,
        )?;

        abs_y = abs_y_end;
    }

    Ok(())
}

#[inline(always)]
fn filter_high_rows_streaming<S: Simd>(
    simd: S,
    codestream: &[u8],
    lh_idx: usize,
    hh_idx: usize,
    resolution: u8,
    component_info: &ComponentInfo,
    bit_plane_ctx: &mut BitPlaneDecodeContext,
    bit_plane_buffers: &mut BitPlaneDecodeBuffers,
    storage: &DecompositionStorage,
    header: &Header,
    max_layers: Option<u8>,
    lh_strip: &mut Vec<f32>,
    hh_strip: &mut Vec<f32>,
    low_row: &mut Vec<f32>,
    high_row: &mut Vec<f32>,
    coefficients: &mut [f32],
    width: usize,
    dst_stride: usize,
    first_row: usize,
    low_first: bool,
    num_low: usize,
    num_high: usize,
    transform: WaveletTransform,
    cancel: Cancel<'_>,
) -> Result<()> {
    let lh_sb = storage
        .sub_bands
        .get(lh_idx)
        .ok_or(DecodeError::Malformed)?;
    let y0 = lh_sb.rect.y0;
    let y1 = lh_sb.rect.y1;
    let cb_height = lh_sb.cb_height;
    let mut filter =
        HorizontalFilter::new(transform, low_first, num_low, num_high, low_row, high_row)?;

    let mut abs_y = y0;
    while abs_y < y1 {
        let next_grid = (abs_y / cb_height + 1).saturating_mul(cb_height);
        let abs_y_end = next_grid.min(y1);
        let v_start = (abs_y - y0) as usize;
        let strip_h = (abs_y_end - abs_y) as usize;

        decode_sub_band_strip(
            codestream,
            lh_idx,
            resolution,
            component_info,
            bit_plane_ctx,
            bit_plane_buffers,
            storage,
            header,
            max_layers,
            abs_y,
            abs_y_end,
            lh_strip,
            cancel,
        )?;
        decode_sub_band_strip(
            codestream,
            hh_idx,
            resolution,
            component_info,
            bit_plane_ctx,
            bit_plane_buffers,
            storage,
            header,
            max_layers,
            abs_y,
            abs_y_end,
            hh_strip,
            cancel,
        )?;

        filter.filter_rows(
            simd,
            lh_strip,
            hh_strip,
            coefficients,
            width,
            dst_stride,
            first_row + v_start * 2,
            strip_h,
            cancel,
        )?;

        abs_y = abs_y_end;
    }

    Ok(())
}

#[inline(always)]
fn lift_band<S: Simd>(
    simd: S,
    target: &mut [f32],
    source: &mut [f32],
    offset: usize,
    f_simd: impl Fn(f32x8<S>, f32x8<S>, f32x8<S>) -> f32x8<S>,
    f_scalar: impl Fn(f32, f32, f32) -> f32,
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

    lift_samples(simd, target, left, right, f_simd, f_scalar);
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
fn filter_vertical<S: Simd>(
    simd: S,
    coefficients: &mut [f32],
    stride: usize,
    rect: IntRect,
    transform: WaveletTransform,
    cancel: Cancel<'_>,
) -> Result<()> {
    let width = rect.width() as usize;
    let height = rect.height() as usize;
    let low_first = rect.y0.is_multiple_of(2);

    if low_first {
        match transform {
            WaveletTransform::Reversible53 if height >= 4 => {
                return filter_vertical_53r_fused(
                    simd,
                    coefficients,
                    width,
                    stride,
                    height,
                    cancel,
                );
            }
            WaveletTransform::Irreversible97 if height >= 8 => {
                return filter_vertical_97i_fused(
                    simd,
                    coefficients,
                    width,
                    stride,
                    height,
                    cancel,
                );
            }
            _ => {}
        }
    }

    let mut filter = VerticalFilter {
        cancel,
        coefficients,
        width,
        stride,
        height,
        low_first,
    };

    filter_1d(simd, transform, &mut filter)
}

#[inline(always)]
fn scale_ring_row<S: Simd, const RING: usize>(
    simd: S,
    ring: &mut [f32],
    width: usize,
    row: usize,
    factor: f32,
) -> Result<()> {
    scale_row(simd, ring, width, width, row & (RING - 1), factor)
}

#[inline(always)]
fn lift_ring_row<S: Simd, const RING: usize>(
    simd: S,
    ring: &mut [f32],
    width: usize,
    row: usize,
    above: usize,
    below: usize,
    f_simd: impl Fn(f32x8<S>, f32x8<S>, f32x8<S>) -> f32x8<S>,
    f_scalar: impl Fn(f32, f32, f32) -> f32,
) -> Result<()> {
    lift_row(
        simd,
        ring,
        width,
        width,
        row & (RING - 1),
        above & (RING - 1),
        below & (RING - 1),
        f_simd,
        f_scalar,
    )
}

#[inline(always)]
fn scale_row<S: Simd>(
    simd: S,
    coefficients: &mut [f32],
    width: usize,
    stride: usize,
    row: usize,
    factor: f32,
) -> Result<()> {
    let slice = coefficients
        .get_mut(row * stride..)
        .and_then(|s| s.get_mut(..width))
        .ok_or(DecodeError::Malformed)?;
    scale(simd, slice, factor);
    Ok(())
}

#[inline(always)]
fn lift_row<S: Simd>(
    simd: S,
    coefficients: &mut [f32],
    width: usize,
    stride: usize,
    row: usize,
    above: usize,
    below: usize,
    f_simd: impl Fn(f32x8<S>, f32x8<S>, f32x8<S>) -> f32x8<S>,
    f_scalar: impl Fn(f32, f32, f32) -> f32,
) -> Result<()> {
    let (rows_before, rest) = coefficients.split_at_mut(row * stride);
    let (current, rows_after) = rest.split_at_mut(width);
    let neighbor = |r: usize| -> Result<&[f32]> {
        if r < row {
            rows_before
                .get(r * stride..)
                .and_then(|s| s.get(..width))
                .ok_or(DecodeError::Malformed)
        } else {
            rows_after
                .get((r - row) * stride - width..)
                .and_then(|s| s.get(..width))
                .ok_or(DecodeError::Malformed)
        }
    };

    lift_samples(
        simd,
        current,
        neighbor(above)?,
        neighbor(below)?,
        f_simd,
        f_scalar,
    );
    Ok(())
}

#[inline(always)]
fn filter_vertical_53r_fused<S: Simd>(
    simd: S,
    coefficients: &mut [f32],
    width: usize,
    stride: usize,
    height: usize,
    cancel: Cancel<'_>,
) -> Result<()> {
    let lift_low_simd =
        |s: f32x8<S>, left: f32x8<S>, right: f32x8<S>| s - ((left + right + 2.0) * 0.25).floor();
    let lift_low_scalar =
        |s: f32, left: f32, right: f32| s - math::mul_add(left + right, 0.25, 0.5).floor();
    let lift_high_simd =
        |s: f32x8<S>, left: f32x8<S>, right: f32x8<S>| s + ((left + right) * 0.5).floor();
    let lift_high_scalar = |s: f32, left: f32, right: f32| s + ((left + right) * 0.5).floor();

    lift_row(
        simd,
        coefficients,
        width,
        stride,
        0,
        1,
        1,
        lift_low_simd,
        lift_low_scalar,
    )?;

    for even in (2..height).step_by(2) {
        cancel.check()?;
        let below = if even + 1 < height {
            even + 1
        } else {
            even - 1
        };
        lift_row(
            simd,
            coefficients,
            width,
            stride,
            even,
            even - 1,
            below,
            lift_low_simd,
            lift_low_scalar,
        )?;
        lift_row(
            simd,
            coefficients,
            width,
            stride,
            even - 1,
            even - 2,
            even,
            lift_high_simd,
            lift_high_scalar,
        )?;
    }

    if height.is_multiple_of(2) {
        let last_odd = height - 1;
        lift_row(
            simd,
            coefficients,
            width,
            stride,
            last_odd,
            last_odd - 1,
            last_odd - 1,
            lift_high_simd,
            lift_high_scalar,
        )?;
    }

    Ok(())
}

#[inline(always)]
fn filter_vertical_97i_fused<S: Simd>(
    simd: S,
    coefficients: &mut [f32],
    width: usize,
    stride: usize,
    height: usize,
    cancel: Cancel<'_>,
) -> Result<()> {
    let neg_alpha = f32x8::splat(simd, NEG_ALPHA);
    let neg_beta = f32x8::splat(simd, NEG_BETA);
    let neg_gamma = f32x8::splat(simd, NEG_GAMMA);
    let neg_delta = f32x8::splat(simd, NEG_DELTA);

    let delta_simd = |s: f32x8<S>, l: f32x8<S>, r: f32x8<S>| (l + r).mul_add(neg_delta, s);
    let delta_scalar = |s: f32, l: f32, r: f32| math::mul_add(l + r, NEG_DELTA, s);
    let gamma_simd = |s: f32x8<S>, l: f32x8<S>, r: f32x8<S>| (l + r).mul_add(neg_gamma, s);
    let gamma_scalar = |s: f32, l: f32, r: f32| math::mul_add(l + r, NEG_GAMMA, s);
    let beta_simd = |s: f32x8<S>, l: f32x8<S>, r: f32x8<S>| (l + r).mul_add(neg_beta, s);
    let beta_scalar = |s: f32, l: f32, r: f32| math::mul_add(l + r, NEG_BETA, s);
    let alpha_simd = |s: f32x8<S>, l: f32x8<S>, r: f32x8<S>| (l + r).mul_add(neg_alpha, s);
    let alpha_scalar = |s: f32, l: f32, r: f32| math::mul_add(l + r, NEG_ALPHA, s);

    for even in (0..height).step_by(2) {
        cancel.check()?;
        scale_row(simd, coefficients, width, stride, even, KAPPA)?;
        if even + 1 < height {
            scale_row(simd, coefficients, width, stride, even + 1, INV_KAPPA)?;
        }

        let above = if even == 0 { 1 } else { even - 1 };
        let below = if even + 1 < height {
            even + 1
        } else {
            even - 1
        };
        lift_row(
            simd,
            coefficients,
            width,
            stride,
            even,
            above,
            below,
            delta_simd,
            delta_scalar,
        )?;

        if even >= 2 {
            lift_row(
                simd,
                coefficients,
                width,
                stride,
                even - 1,
                even - 2,
                even,
                gamma_simd,
                gamma_scalar,
            )?;

            let beta_row = even - 2;
            let beta_above = if beta_row == 0 { 1 } else { beta_row - 1 };
            lift_row(
                simd,
                coefficients,
                width,
                stride,
                beta_row,
                beta_above,
                beta_row + 1,
                beta_simd,
                beta_scalar,
            )?;
        }

        if even >= 4 {
            lift_row(
                simd,
                coefficients,
                width,
                stride,
                even - 3,
                even - 4,
                even - 2,
                alpha_simd,
                alpha_scalar,
            )?;
        }
    }

    let last_even = (height - 1) & !1;
    if height.is_multiple_of(2) {
        let last_odd = height - 1;
        lift_row(
            simd,
            coefficients,
            width,
            stride,
            last_odd,
            last_even,
            last_even,
            gamma_simd,
            gamma_scalar,
        )?;
        lift_row(
            simd,
            coefficients,
            width,
            stride,
            last_even,
            last_even - 1,
            last_odd,
            beta_simd,
            beta_scalar,
        )?;
        lift_row(
            simd,
            coefficients,
            width,
            stride,
            last_even - 1,
            last_even - 2,
            last_even,
            alpha_simd,
            alpha_scalar,
        )?;
        lift_row(
            simd,
            coefficients,
            width,
            stride,
            last_odd,
            last_even,
            last_even,
            alpha_simd,
            alpha_scalar,
        )?;
    } else {
        lift_row(
            simd,
            coefficients,
            width,
            stride,
            last_even,
            last_even - 1,
            last_even - 1,
            beta_simd,
            beta_scalar,
        )?;
        lift_row(
            simd,
            coefficients,
            width,
            stride,
            last_even - 1,
            last_even - 2,
            last_even,
            alpha_simd,
            alpha_scalar,
        )?;
    }

    Ok(())
}

#[inline(always)]
fn filter_1d<S: Simd>(
    simd: S,
    transform: WaveletTransform,
    signal: &mut impl Lifting<S>,
) -> Result<()> {
    if signal.num_samples() == 1 {
        return signal.scale_high(simd, 0.5);
    }

    match transform {
        WaveletTransform::Reversible53 => filter_53r(simd, signal),
        WaveletTransform::Irreversible97 => filter_97i(simd, signal),
    }
}

#[inline(always)]
fn filter_53r<S: Simd>(simd: S, signal: &mut impl Lifting<S>) -> Result<()> {
    signal.lift_low(
        simd,
        #[inline(always)]
        |s, left, right| s - ((left + right + 2.0) * 0.25).floor(),
        #[inline(always)]
        |s, left, right| s - math::mul_add(left + right, 0.25, 0.5).floor(),
    )?;

    signal.lift_high(
        simd,
        #[inline(always)]
        |s, left, right| s + ((left + right) * 0.5).floor(),
        #[inline(always)]
        |s, left, right| s + ((left + right) * 0.5).floor(),
    )?;

    Ok(())
}

#[inline(always)]
fn filter_97i<S: Simd>(simd: S, signal: &mut impl Lifting<S>) -> Result<()> {
    let neg_alpha = f32x8::splat(simd, NEG_ALPHA);
    let neg_beta = f32x8::splat(simd, NEG_BETA);
    let neg_gamma = f32x8::splat(simd, NEG_GAMMA);
    let neg_delta = f32x8::splat(simd, NEG_DELTA);

    signal.scale_low(simd, KAPPA)?;
    signal.scale_high(simd, INV_KAPPA)?;

    signal.lift_low(
        simd,
        #[inline(always)]
        |s, left, right| (left + right).mul_add(neg_delta, s),
        #[inline(always)]
        |s, left, right| math::mul_add(left + right, NEG_DELTA, s),
    )?;

    signal.lift_high(
        simd,
        #[inline(always)]
        |s, left, right| (left + right).mul_add(neg_gamma, s),
        #[inline(always)]
        |s, left, right| math::mul_add(left + right, NEG_GAMMA, s),
    )?;

    signal.lift_low(
        simd,
        #[inline(always)]
        |s, left, right| (left + right).mul_add(neg_beta, s),
        #[inline(always)]
        |s, left, right| math::mul_add(left + right, NEG_BETA, s),
    )?;

    signal.lift_high(
        simd,
        #[inline(always)]
        |s, left, right| (left + right).mul_add(neg_alpha, s),
        #[inline(always)]
        |s, left, right| math::mul_add(left + right, NEG_ALPHA, s),
    )?;

    Ok(())
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
