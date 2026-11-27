use std::vec;
use std::vec::Vec;

use super::arithmetic_decoder::{ArithmeticDecoder, ArithmeticDecoderContext};
use super::build::{CodeBlock, SubBandType};
use super::codestream::CodeBlockStyle;
use super::decode::DecompositionStorage;
use crate::error::{DecodeError, Result, bail};
use crate::reader::BitReader;

pub(crate) fn decode(
    codestream: &[u8],
    code_block: &CodeBlock,
    sub_band_type: SubBandType,
    total_bitplanes: u8,
    style: &CodeBlockStyle,
    bit_plane_ctx: &mut BitPlaneDecodeContext,
    bit_plane_buffers: &mut BitPlaneDecodeBuffers,
    storage: &DecompositionStorage,
    max_layers: Option<u8>,
    strict: bool,
) -> Result<bool> {
    bit_plane_ctx.prepare_header(code_block, sub_band_type, style, total_bitplanes, strict)?;

    decode_inner(
        codestream,
        code_block,
        storage,
        bit_plane_ctx,
        bit_plane_buffers,
        max_layers,
    )
    .ok_or(DecodeError::Malformed)
}

fn decode_inner(
    codestream: &[u8],
    code_block: &CodeBlock,
    storage: &DecompositionStorage,
    ctx: &mut BitPlaneDecodeContext,
    bp_buffers: &mut BitPlaneDecodeBuffers,
    max_layers: Option<u8>,
) -> Option<bool> {
    if code_block.number_of_coding_passes == 0 || ctx.max_coding_passes == 0 {
        return Some(false);
    }

    bp_buffers.reset();

    let mut last_segment_idx = 0;
    let mut coding_passes = 0;
    let layer_limit = max_layers.map_or(usize::MAX, usize::from);

    for layer in storage
        .layers
        .get(code_block.layers_range())?
        .iter()
        .take(layer_limit)
    {
        if let Some(range) = layer.segments() {
            let layer_segments = storage.segments.get(range)?;
            for segment in layer_segments {
                if segment.idx != last_segment_idx {
                    assert_eq!(segment.idx, last_segment_idx + 1);

                    bp_buffers
                        .segment_ranges
                        .push(bp_buffers.combined_layers.len());
                    bp_buffers.segment_coding_passes.push(coding_passes);
                    last_segment_idx += 1;
                }

                let end = segment
                    .data_start
                    .checked_add(segment.data_length as usize)?;
                let bytes = codestream.get(segment.data_start..end)?;
                bp_buffers.combined_layers.extend_from_slice(bytes);
                coding_passes += segment.coding_pases;
            }
        }
    }

    if max_layers.is_none() {
        assert_eq!(coding_passes, code_block.number_of_coding_passes);
    }

    if coding_passes == 0 {
        return Some(false);
    }

    bp_buffers
        .segment_ranges
        .push(bp_buffers.combined_layers.len());
    bp_buffers.segment_coding_passes.push(coding_passes);

    let is_normal_mode =
        !ctx.style.selective_arithmetic_coding_bypass && !ctx.style.termination_on_each_pass;
    let use_fast_path = is_normal_mode && ctx.can_use_fast_path();
    ctx.prepare_buffers(use_fast_path);

    if is_normal_mode {
        let mut decoder = ArithmeticDecoder::new(&bp_buffers.combined_layers);
        let end = coding_passes.min(ctx.max_coding_passes);

        if use_fast_path {
            fast_path::handle_coding_passes(0, end, ctx, &mut decoder)?;
        } else {
            handle_coding_passes(0, end, ctx, &mut decoder)?;
        }
    } else {
        for (passes, ranges) in bp_buffers
            .segment_coding_passes
            .windows(2)
            .zip(bp_buffers.segment_ranges.windows(2))
        {
            let [start_coding_pass, end_coding_pass] = *passes else {
                continue;
            };
            let [range_start, range_end] = *ranges else {
                continue;
            };
            let end_coding_pass = end_coding_pass.min(ctx.max_coding_passes);

            let data = bp_buffers.combined_layers.get(range_start..range_end)?;

            let use_arithmetic = if ctx.style.selective_arithmetic_coding_bypass {
                if start_coding_pass <= 9 {
                    true
                } else {
                    start_coding_pass.is_multiple_of(3)
                }
            } else {
                true
            };

            if use_arithmetic {
                let mut decoder = ArithmeticDecoder::new(data);
                handle_coding_passes(start_coding_pass, end_coding_pass, ctx, &mut decoder)?;
            } else {
                let mut decoder = BypassDecoder::new(data, ctx.strict);
                handle_coding_passes(start_coding_pass, end_coding_pass, ctx, &mut decoder)?;
            }
        }
    }

    Some(true)
}

fn handle_coding_passes(
    start: u8,
    end: u8,
    ctx: &mut BitPlaneDecodeContext,
    decoder: &mut impl BitDecoder,
) -> Option<()> {
    let reset_context_probabilities = ctx.style.reset_context_probabilities;

    for coding_pass in start..end {
        let current_bitplane = coding_pass.div_ceil(3);
        ctx.current_bit_position = ctx.bitplanes - 1 - current_bitplane;

        match coding_pass % 3 {
            0 => {
                cleanup_pass(ctx, decoder)?;

                if ctx.style.segmentation_symbols {
                    let b0 = decoder.read_bit(ctx.arithmetic_decoder_context(18)?)?;
                    let b1 = decoder.read_bit(ctx.arithmetic_decoder_context(18)?)?;
                    let b2 = decoder.read_bit(ctx.arithmetic_decoder_context(18)?)?;
                    let b3 = decoder.read_bit(ctx.arithmetic_decoder_context(18)?)?;

                    if (b0 != 1 || b1 != 0 || b2 != 1 || b3 != 0) && ctx.strict {
                        return None;
                    }
                }

                ctx.reset_for_next_bitplane();
            }
            1 => {
                significance_propagation_pass(ctx, decoder)?;
            }
            2 => {
                magnitude_refinement_pass(ctx, decoder)?;
            }
            _ => unreachable!(),
        }

        if reset_context_probabilities {
            ctx.reset_contexts();
        }
    }

    Some(())
}

const SIGNIFICANCE_SHIFT: u8 = 7;
const HAS_MAGNITUDE_REFINEMENT_SHIFT: u8 = 6;
const HAS_ZERO_CODING_SHIFT: u8 = 5;
const DECODED_BIT_POSITION_MASK: u8 = 0x1f;
const CLEANUP_SKIP_MASK: u8 = (1 << SIGNIFICANCE_SHIFT) | (1 << HAS_ZERO_CODING_SHIFT);
const REFINEMENT_PASS_MASK: u8 = (1 << SIGNIFICANCE_SHIFT) | (1 << HAS_ZERO_CODING_SHIFT);

#[derive(Default, Copy, Clone)]
pub(crate) struct CoefficientState(u8);

impl CoefficientState {
    #[inline(always)]
    fn set_significant(&mut self) {
        self.0 |= 1_u8 << SIGNIFICANCE_SHIFT;
    }

    #[inline(always)]
    fn set_zero_coded(&mut self, value: u8) {
        debug_assert!(value < 2);

        self.0 &= !(1_u8 << HAS_ZERO_CODING_SHIFT);
        self.0 |= value << HAS_ZERO_CODING_SHIFT;
    }

    #[inline(always)]
    fn set_magnitude_refined(&mut self) {
        self.0 |= 1_u8 << HAS_MAGNITUDE_REFINEMENT_SHIFT;
    }

    #[inline(always)]
    fn set_decoded_bit_position(&mut self, position: u8) {
        debug_assert!(position <= DECODED_BIT_POSITION_MASK);

        self.0 = (self.0 & !DECODED_BIT_POSITION_MASK) | position;
    }

    #[inline(always)]
    fn decoded_bit_position(&self) -> u8 {
        self.0 & DECODED_BIT_POSITION_MASK
    }

    #[inline(always)]
    fn is_significant(&self) -> bool {
        self.significance() == 1
    }

    #[inline(always)]
    fn significance(&self) -> u8 {
        (self.0 >> SIGNIFICANCE_SHIFT) & 1
    }

    #[inline(always)]
    fn magnitude_refinement(&self) -> u8 {
        (self.0 >> HAS_MAGNITUDE_REFINEMENT_SHIFT) & 1
    }

    #[inline(always)]
    fn should_decode_cleanup(&self) -> bool {
        self.0 & CLEANUP_SKIP_MASK == 0
    }

    #[inline(always)]
    fn should_decode_refinement(&self) -> bool {
        self.0 & REFINEMENT_PASS_MASK == 1 << SIGNIFICANCE_SHIFT
    }
}

#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct Coefficient(u32);

impl Coefficient {
    pub(crate) fn reconstructed(
        &self,
        state: &CoefficientState,
        irreversible: bool,
        roi_shift: u8,
    ) -> f32 {
        let mut magnitude = self.0 & !0x80000000;
        let mut bit_position = state.decoded_bit_position();

        let roi_magnitude = magnitude.checked_shr(roi_shift as u32).unwrap_or(0);
        if roi_magnitude != 0 {
            magnitude = roi_magnitude;
            bit_position = bit_position.saturating_sub(roi_shift);
        }

        let mut magnitude = magnitude as i32;
        magnitude *= 1 - 2 * (self.sign() as i32);

        if magnitude != 0 && (irreversible || bit_position != 0) {
            let offset = 0.5 * (1_u32 << bit_position) as f32;
            return magnitude as f32 + if magnitude > 0 { offset } else { -offset };
        }

        magnitude as f32
    }

    fn set_sign(&mut self, sign: u8) {
        self.0 |= (sign as u32) << 31;
    }

    fn sign(&self) -> u32 {
        (self.0 >> 31) & 1
    }

    fn push_bit_at(&mut self, bit: u32, position: u8) {
        self.0 |= bit << position;
    }
}

const COEFFICIENTS_PADDING: u32 = 1;

#[derive(Default, Copy, Clone)]
struct NeighborSignificances(u8);

impl NeighborSignificances {
    fn set_top_left(&mut self) {
        self.0 |= 1 << 7;
    }

    fn set_top(&mut self) {
        self.0 |= 1 << 6;
    }

    fn set_top_right(&mut self) {
        self.0 |= 1 << 5;
    }

    fn set_left(&mut self) {
        self.0 |= 1 << 4;
    }

    fn set_bottom_left(&mut self) {
        self.0 |= 1 << 3;
    }

    fn set_right(&mut self) {
        self.0 |= 1 << 2;
    }

    fn set_bottom_right(&mut self) {
        self.0 |= 1 << 1;
    }

    fn set_bottom(&mut self) {
        self.0 |= 1;
    }

    fn all(&self) -> u8 {
        self.0
    }

    fn all_without_bottom(&self) -> u8 {
        self.0 & 0b11110100
    }
}

#[derive(Default)]
pub(crate) struct BitPlaneDecodeBuffers {
    combined_layers: Vec<u8>,
    segment_ranges: Vec<usize>,
    segment_coding_passes: Vec<u8>,
}

impl BitPlaneDecodeBuffers {
    fn reset(&mut self) {
        self.combined_layers.clear();
        self.segment_ranges.clear();
        self.segment_coding_passes.clear();

        self.segment_ranges.push(0);
        self.segment_coding_passes.push(0);
    }
}

pub(crate) struct BitPlaneDecodeContext {
    coefficient_states: Vec<CoefficientState>,
    stripe_flags: Vec<u32>,
    stripe_mags_u16: Vec<[u16; 4]>,
    stripe_mags_u32: Vec<[u32; 4]>,
    neighbor_significances: Vec<NeighborSignificances>,
    coefficients: Vec<Coefficient>,
    width: u32,
    padded_width: u32,
    height: u32,
    active_coefficients: usize,
    active_stripe_flags: usize,
    active_stripe_mags: usize,
    used_fast_path: bool,
    use_u16_mags: bool,
    last_bit_position: u8,
    ended_on_spp: bool,
    style: CodeBlockStyle,
    bitplanes: u8,
    strict: bool,
    max_coding_passes: u8,
    sub_band_type: SubBandType,
    contexts: [ArithmeticDecoderContext; 32],
    current_bit_position: u8,
}

impl Default for BitPlaneDecodeContext {
    fn default() -> Self {
        Self {
            coefficient_states: vec![],
            stripe_flags: vec![],
            stripe_mags_u16: vec![],
            stripe_mags_u32: vec![],
            coefficients: vec![],
            neighbor_significances: vec![],
            width: 0,
            padded_width: COEFFICIENTS_PADDING * 2,
            height: 0,
            active_coefficients: 0,
            active_stripe_flags: 0,
            active_stripe_mags: 0,
            used_fast_path: false,
            use_u16_mags: false,
            last_bit_position: 0,
            ended_on_spp: false,
            style: CodeBlockStyle::default(),
            bitplanes: 0,
            max_coding_passes: 0,
            strict: false,
            sub_band_type: SubBandType::LowLow,
            contexts: [ArithmeticDecoderContext::default(); 32],
            current_bit_position: 0,
        }
    }
}

impl BitPlaneDecodeContext {
    fn can_use_fast_path(&self) -> bool {
        !self.style.vertically_causal_context
    }

    fn prepare_header(
        &mut self,
        code_block: &CodeBlock,
        sub_band_type: SubBandType,
        code_block_style: &CodeBlockStyle,
        total_bitplanes: u8,
        strict: bool,
    ) -> Result<()> {
        let (width, height) = (code_block.rect.width(), code_block.rect.height());
        let padded_width = width + COEFFICIENTS_PADDING * 2;
        let padded_height = height + COEFFICIENTS_PADDING * 2;

        self.width = width;
        self.padded_width = padded_width;
        self.height = height;
        self.active_coefficients = padded_width as usize * padded_height as usize;
        self.active_stripe_flags = (height.div_ceil(4) + 2) as usize * padded_width as usize;
        self.sub_band_type = sub_band_type;
        self.style = *code_block_style;
        self.strict = strict;

        self.bitplanes = if strict {
            total_bitplanes
                .checked_sub(code_block.missing_bit_planes)
                .ok_or(DecodeError::Malformed)?
        } else {
            total_bitplanes.saturating_sub(code_block.missing_bit_planes)
        };

        self.max_coding_passes = if self.bitplanes == 0 {
            0
        } else {
            1 + 3 * (self.bitplanes - 1)
        };

        if self.max_coding_passes < code_block.number_of_coding_passes && strict {
            bail!(DecodeError::Malformed);
        }

        Ok(())
    }

    fn prepare_buffers(&mut self, use_fast_path: bool) {
        if use_fast_path {
            let stripe_len = self.active_stripe_flags;
            if self.stripe_flags.len() < stripe_len {
                self.stripe_flags.resize(stripe_len, 0);
            }
            if let Some(flags) = self.stripe_flags.get_mut(..stripe_len) {
                flags.fill(0);
            }
            let mag_len = self.height.div_ceil(4) as usize * self.width as usize;
            self.active_stripe_mags = mag_len;
            self.use_u16_mags = self.bitplanes <= 16;
            if self.use_u16_mags {
                if self.stripe_mags_u16.len() < mag_len {
                    if self.stripe_mags_u16.is_empty() && mag_len < 4 {
                        self.stripe_mags_u16.reserve_exact(mag_len);
                    }
                    self.stripe_mags_u16.resize(mag_len, [0; 4]);
                }
            } else if self.stripe_mags_u32.len() < mag_len {
                if self.stripe_mags_u32.is_empty() && mag_len < 4 {
                    self.stripe_mags_u32.reserve_exact(mag_len);
                }
                self.stripe_mags_u32.resize(mag_len, [0; 4]);
            }
        } else {
            let num_coefficients = self.active_coefficients;

            if self.coefficients.len() < num_coefficients {
                self.coefficients
                    .resize(num_coefficients, Coefficient::default());
            }
            if let Some(coeffs) = self.coefficients.get_mut(..num_coefficients) {
                coeffs.fill(Coefficient::default());
            }
            if self.coefficient_states.len() < num_coefficients {
                self.coefficient_states
                    .resize(num_coefficients, CoefficientState::default());
            }
            if let Some(states) = self.coefficient_states.get_mut(..num_coefficients) {
                states.fill(CoefficientState::default());
            }
            if self.neighbor_significances.len() < num_coefficients {
                self.neighbor_significances
                    .resize(num_coefficients, NeighborSignificances::default());
            }
            if let Some(neighbors) = self.neighbor_significances.get_mut(..num_coefficients) {
                neighbors.fill(NeighborSignificances::default());
            }
        }

        self.used_fast_path = use_fast_path;
        self.reset_contexts();
    }

    pub(crate) fn write_reconstructed(
        &self,
        dst: &mut [f32],
        mut base_idx: usize,
        sb_width: usize,
        irreversible: bool,
        roi_shift: u8,
        dequantization_step: f32,
    ) -> Result<()> {
        let pad = COEFFICIENTS_PADDING as usize;
        let width = self.width as usize;
        let height = self.height as usize;
        let stride = self.padded_width as usize;

        if self.used_fast_path {
            if self.use_u16_mags {
                fast_path::write_reconstructed(
                    &self.stripe_flags,
                    &self.stripe_mags_u16,
                    dst,
                    base_idx,
                    sb_width,
                    width,
                    height,
                    stride,
                    self.last_bit_position,
                    self.ended_on_spp,
                    irreversible,
                    roi_shift,
                    dequantization_step,
                    u32::from,
                )?;
            } else {
                fast_path::write_reconstructed(
                    &self.stripe_flags,
                    &self.stripe_mags_u32,
                    dst,
                    base_idx,
                    sb_width,
                    width,
                    height,
                    stride,
                    self.last_bit_position,
                    self.ended_on_spp,
                    irreversible,
                    roi_shift,
                    dequantization_step,
                    |m| m,
                )?;
            }
        } else {
            for y in 0..height {
                let row_start = (y + pad) * stride + pad;
                let coeff_row = self
                    .coefficients
                    .get(row_start..)
                    .and_then(|s| s.get(..width))
                    .ok_or(DecodeError::Malformed)?;
                let state_row = self
                    .coefficient_states
                    .get(row_start..)
                    .and_then(|s| s.get(..width))
                    .ok_or(DecodeError::Malformed)?;
                let out_row = dst
                    .get_mut(base_idx..)
                    .and_then(|s| s.get_mut(..width))
                    .ok_or(DecodeError::Malformed)?;

                for ((output, coefficient), coefficient_state) in
                    out_row.iter_mut().zip(coeff_row).zip(state_row)
                {
                    *output = coefficient.reconstructed(coefficient_state, irreversible, roi_shift)
                        * dequantization_step;
                }

                base_idx += sb_width;
            }
        }

        Ok(())
    }

    fn set_sign(&mut self, pos: Position, sign: u8) {
        if let Some(coeff) = self.coefficients.get_mut(pos.index(self.padded_width)) {
            coeff.set_sign(sign);
        }
    }

    fn arithmetic_decoder_context(
        &mut self,
        ctx_label: u8,
    ) -> Option<&mut ArithmeticDecoderContext> {
        self.contexts.get_mut(ctx_label as usize)
    }

    fn reset_contexts(&mut self) {
        reset_context_array(&mut self.contexts);
    }

    fn reset_for_next_bitplane(&mut self) {
        if let Some(states) = self.coefficient_states.get_mut(..self.active_coefficients) {
            for el in states {
                el.set_zero_coded(0);
            }
        }
    }

    #[inline(always)]
    fn is_significant(&self, position: Position) -> bool {
        self.coefficient_states
            .get(position.index(self.padded_width))
            .is_some_and(CoefficientState::is_significant)
    }

    #[inline(always)]
    fn set_significant(&mut self, position: Position) {
        let idx = position.index(self.padded_width);
        if let Some(state) = self.coefficient_states.get_mut(idx) {
            debug_assert!(!state.is_significant());
            state.set_significant();
            state.set_decoded_bit_position(self.current_bit_position);
        }

        if let Some(n) = self
            .neighbor_significances
            .get_mut(position.top_left().index(self.padded_width))
        {
            n.set_bottom_right();
        }
        if let Some(n) = self
            .neighbor_significances
            .get_mut(position.top().index(self.padded_width))
        {
            n.set_bottom();
        }
        if let Some(n) = self
            .neighbor_significances
            .get_mut(position.top_right().index(self.padded_width))
        {
            n.set_bottom_left();
        }
        if let Some(n) = self
            .neighbor_significances
            .get_mut(position.left().index(self.padded_width))
        {
            n.set_right();
        }
        if let Some(n) = self
            .neighbor_significances
            .get_mut(position.right().index(self.padded_width))
        {
            n.set_left();
        }
        if let Some(n) = self
            .neighbor_significances
            .get_mut(position.bottom_left().index(self.padded_width))
        {
            n.set_top_right();
        }
        if let Some(n) = self
            .neighbor_significances
            .get_mut(position.bottom().index(self.padded_width))
        {
            n.set_top();
        }
        if let Some(n) = self
            .neighbor_significances
            .get_mut(position.bottom_right().index(self.padded_width))
        {
            n.set_top_left();
        }
    }

    #[inline(always)]
    fn set_zero_coded(&mut self, position: Position) {
        if let Some(state) = self
            .coefficient_states
            .get_mut(position.index(self.padded_width))
        {
            state.set_zero_coded(1);
        }
    }

    #[inline(always)]
    fn set_magnitude_refined(&mut self, position: Position) {
        if let Some(state) = self
            .coefficient_states
            .get_mut(position.index(self.padded_width))
        {
            state.set_magnitude_refined();
            state.set_decoded_bit_position(self.current_bit_position);
        }
    }

    #[inline(always)]
    fn magnitude_refinement(&self, position: Position) -> u8 {
        self.coefficient_states
            .get(position.index(self.padded_width))
            .map_or(0, CoefficientState::magnitude_refinement)
    }

    #[inline(always)]
    fn should_decode_cleanup(&self, position: Position) -> bool {
        self.coefficient_states
            .get(position.index(self.padded_width))
            .is_some_and(CoefficientState::should_decode_cleanup)
    }

    #[inline(always)]
    fn should_decode_refinement(&self, position: Position) -> bool {
        self.coefficient_states
            .get(position.index(self.padded_width))
            .is_some_and(CoefficientState::should_decode_refinement)
    }

    #[inline(always)]
    fn push_magnitude_bit(&mut self, position: Position, bit: u32) {
        let idx = position.index(self.padded_width);
        if let Some(coeff) = self.coefficients.get_mut(idx) {
            coeff.push_bit_at(bit, self.current_bit_position);
        }
    }

    #[inline(always)]
    fn sign(&self, position: Position) -> u8 {
        self.coefficients
            .get(position.index(self.padded_width))
            .map_or(0, |c| c.sign() as u8)
    }

    #[inline(always)]
    fn neighbor_in_next_stripe(&self, pos: Position) -> bool {
        let neighbor = pos.bottom();
        neighbor.real_y() < self.height && (neighbor.real_y() >> 2) > (pos.real_y() >> 2)
    }

    #[inline(always)]
    fn neighborhood_significance_states(&self, pos: Position) -> u8 {
        let Some(neighbors) = self
            .neighbor_significances
            .get(pos.index(self.padded_width))
        else {
            return 0;
        };

        if self.style.vertically_causal_context && self.neighbor_in_next_stripe(pos) {
            neighbors.all_without_bottom()
        } else {
            neighbors.all()
        }
    }
}

fn reset_context_array(contexts: &mut [ArithmeticDecoderContext; 32]) {
    for context in contexts.iter_mut() {
        context.reset();
    }

    contexts[0].reset_with_index(4);
    contexts[17].reset_with_index(3);
    contexts[18].reset_with_index(46);
}

fn cleanup_pass(ctx: &mut BitPlaneDecodeContext, decoder: &mut impl BitDecoder) -> Option<()> {
    for_each_position(
        ctx.width,
        ctx.height,
        #[inline(always)]
        |cur_pos| {
            if ctx.should_decode_cleanup(*cur_pos) {
                let use_rl = cur_pos.real_y() % 4 == 0
                    && (ctx.height - cur_pos.real_y()) >= 4
                    && ctx.neighborhood_significance_states(*cur_pos) == 0
                    && ctx.neighborhood_significance_states(Position::new_index(
                        cur_pos.index_x,
                        cur_pos.index_y + 1,
                    )) == 0
                    && ctx.neighborhood_significance_states(Position::new_index(
                        cur_pos.index_x,
                        cur_pos.index_y + 2,
                    )) == 0
                    && ctx.neighborhood_significance_states(Position::new_index(
                        cur_pos.index_x,
                        cur_pos.index_y + 3,
                    )) == 0;

                let bit = if use_rl {
                    let bit = decoder.read_bit(ctx.arithmetic_decoder_context(17)?)?;

                    if bit == 0 {
                        for _ in 0..3 {
                            cur_pos.index_y += 1;
                        }

                        return Some(());
                    } else {
                        let mut num_zeroes =
                            decoder.read_bit(ctx.arithmetic_decoder_context(18)?)?;
                        num_zeroes = (num_zeroes << 1)
                            | decoder.read_bit(ctx.arithmetic_decoder_context(18)?)?;

                        for _ in 0..num_zeroes {
                            cur_pos.index_y += 1;
                        }

                        1
                    }
                } else {
                    let ctx_label = context_label_zero_coding(*cur_pos, ctx);
                    decoder.read_bit(ctx.arithmetic_decoder_context(ctx_label)?)?
                };

                if bit == 1 {
                    ctx.push_magnitude_bit(*cur_pos, bit);
                    decode_sign_bit(*cur_pos, ctx, decoder);
                    ctx.set_significant(*cur_pos);
                }
            }

            Some(())
        },
    )
}

fn significance_propagation_pass(
    ctx: &mut BitPlaneDecodeContext,
    decoder: &mut impl BitDecoder,
) -> Option<()> {
    for_each_position(
        ctx.width,
        ctx.height,
        #[inline(always)]
        |cur_pos| {
            if !ctx.is_significant(*cur_pos) && ctx.neighborhood_significance_states(*cur_pos) != 0
            {
                let ctx_label = context_label_zero_coding(*cur_pos, ctx);
                let bit = decoder.read_bit(ctx.arithmetic_decoder_context(ctx_label)?)?;
                ctx.set_zero_coded(*cur_pos);

                if bit == 1 {
                    ctx.push_magnitude_bit(*cur_pos, bit);
                    decode_sign_bit(*cur_pos, ctx, decoder)?;
                    ctx.set_significant(*cur_pos);
                }
            }

            Some(())
        },
    )
}

fn magnitude_refinement_pass(
    ctx: &mut BitPlaneDecodeContext,
    decoder: &mut impl BitDecoder,
) -> Option<()> {
    for_each_position(
        ctx.width,
        ctx.height,
        #[inline(always)]
        |cur_pos| {
            if ctx.should_decode_refinement(*cur_pos) {
                let ctx_label = context_label_magnitude_refinement_coding(*cur_pos, ctx);
                let bit = decoder.read_bit(ctx.arithmetic_decoder_context(ctx_label)?)?;
                if bit == 1 {
                    ctx.push_magnitude_bit(*cur_pos, bit);
                }
                ctx.set_magnitude_refined(*cur_pos);
            }

            Some(())
        },
    )
}

fn for_each_position(
    width: u32,
    height: u32,
    mut action: impl FnMut(&mut Position) -> Option<()>,
) -> Option<()> {
    for base_row in (0..height).step_by(4) {
        for x in 0..width {
            let mut cur_pos = Position::new(x, base_row);
            while cur_pos.real_y() < (base_row + 4).min(height) {
                action(&mut cur_pos)?;
                cur_pos.index_y += 1;
            }
        }
    }

    Some(())
}

#[rustfmt::skip]
const SIGN_CONTEXT_LOOKUP: [(u8, u8); 256] = [
    (9,0), (10,0), (10,1), (0,0), (12,0), (13,0), (11,0), (0,0), (12,1), (11,1), 
    (13,1), (0,0), (0,0), (0,0), (0,0), (0,0), (12,0), (13,0), (11,0), (0,0), 
    (12,0), (13,0), (11,0), (0,0), (9,0), (10,0), (10,1), (0,0), (0,0), (0,0), 
    (0,0), (0,0), (12,1), (11,1), (13,1), (0,0), (9,0), (10,0), (10,1), (0,0), 
    (12,1), (11,1), (13,1), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0),
    (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0),
    (0,0), (0,0), (0,0), (10,0), (10,0), (9,0), (0,0), (13,0), (13,0), (12,0), 
    (0,0), (11,1), (11,1), (12,1), (0,0), (0,0), (0,0), (0,0), (0,0), (13,0), 
    (13,0), (12,0), (0,0), (13,0), (13,0), (12,0), (0,0), (10,0), (10,0), (9,0), 
    (0,0), (0,0), (0,0), (0,0), (0,0), (11,1), (11,1), (12,1), (0,0), (10,0), 
    (10,0), (9,0), (0,0), (11,1), (11,1), (12,1), (0,0), (0,0), (0,0), (0,0), 
    (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), 
    (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (10,1), (9,0), (10,1), (0,0), 
    (11,0), (12,0), (11,0), (0,0), (13,1), (12,1), (13,1), (0,0), (0,0), (0,0), 
    (0,0), (0,0), (11,0), (12,0), (11,0), (0,0), (11,0), (12,0), (11,0), (0,0), 
    (10,1), (9,0), (10,1), (0,0), (0,0), (0,0), (0,0), (0,0), (13,1), (12,1), 
    (13,1), (0,0), (10,1), (9,0), (10,1), (0,0), (13,1), (12,1), (13,1), (0,0),
    (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), 
    (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), 
    (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), 
    (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), 
    (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), 
    (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), 
    (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), 
    (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0),
];

#[rustfmt::skip]
const ZERO_CTX_LL_LH_LOOKUP: [u8; 256] = [
    0, 3, 1, 3, 5, 7, 6, 7, 1, 3, 2, 3, 6, 7, 6, 7, 5, 7, 6, 7, 8, 8, 8, 8, 6,
    7, 6, 7, 8, 8, 8, 8, 1, 3, 2, 3, 6, 7, 6, 7, 2, 3, 2, 3, 6, 7, 6, 7, 6, 7,
    6, 7, 8, 8, 8, 8, 6, 7, 6, 7, 8, 8, 8, 8, 3, 4, 3, 4, 7, 7, 7, 7, 3, 4, 3,
    4, 7, 7, 7, 7, 7, 7, 7, 7, 8, 8, 8, 8, 7, 7, 7, 7, 8, 8, 8, 8, 3, 4, 3, 4,
    7, 7, 7, 7, 3, 4, 3, 4, 7, 7, 7, 7, 7, 7, 7, 7, 8, 8, 8, 8, 7, 7, 7, 7, 8,
    8, 8, 8, 1, 3, 2, 3, 6, 7, 6, 7, 2, 3, 2, 3, 6, 7, 6, 7, 6, 7, 6, 7, 8, 8,
    8, 8, 6, 7, 6, 7, 8, 8, 8, 8, 2, 3, 2, 3, 6, 7, 6, 7, 2, 3, 2, 3, 6, 7, 6,
    7, 6, 7, 6, 7, 8, 8, 8, 8, 6, 7, 6, 7, 8, 8, 8, 8, 3, 4, 3, 4, 7, 7, 7, 7,
    3, 4, 3, 4, 7, 7, 7, 7, 7, 7, 7, 7, 8, 8, 8, 8, 7, 7, 7, 7, 8, 8, 8, 8, 3,
    4, 3, 4, 7, 7, 7, 7, 3, 4, 3, 4, 7, 7, 7, 7, 7, 7, 7, 7, 8, 8, 8, 8, 7, 7,
    7, 7, 8, 8, 8, 8,
];

#[rustfmt::skip]
const ZERO_CTX_HL_LOOKUP: [u8; 256] = [
    0, 5, 1, 6, 3, 7, 3, 7, 1, 6, 2, 6, 3, 7, 3, 7, 3, 7, 3, 7, 4, 7, 4, 7, 3,
    7, 3, 7, 4, 7, 4, 7, 1, 6, 2, 6, 3, 7, 3, 7, 2, 6, 2, 6, 3, 7, 3, 7, 3, 7,
    3, 7, 4, 7, 4, 7, 3, 7, 3, 7, 4, 7, 4, 7, 5, 8, 6, 8, 7, 8, 7, 8, 6, 8, 6,
    8, 7, 8, 7, 8, 7, 8, 7, 8, 7, 8, 7, 8, 7, 8, 7, 8, 7, 8, 7, 8, 6, 8, 6, 8,
    7, 8, 7, 8, 6, 8, 6, 8, 7, 8, 7, 8, 7, 8, 7, 8, 7, 8, 7, 8, 7, 8, 7, 8, 7,
    8, 7, 8, 1, 6, 2, 6, 3, 7, 3, 7, 2, 6, 2, 6, 3, 7, 3, 7, 3, 7, 3, 7, 4, 7,
    4, 7, 3, 7, 3, 7, 4, 7, 4, 7, 2, 6, 2, 6, 3, 7, 3, 7, 2, 6, 2, 6, 3, 7, 3,
    7, 3, 7, 3, 7, 4, 7, 4, 7, 3, 7, 3, 7, 4, 7, 4, 7, 6, 8, 6, 8, 7, 8, 7, 8,
    6, 8, 6, 8, 7, 8, 7, 8, 7, 8, 7, 8, 7, 8, 7, 8, 7, 8, 7, 8, 7, 8, 7, 8, 6,
    8, 6, 8, 7, 8, 7, 8, 6, 8, 6, 8, 7, 8, 7, 8, 7, 8, 7, 8, 7, 8, 7, 8, 7, 8,
    7, 8, 7, 8, 7, 8,
];

#[rustfmt::skip]
const ZERO_CTX_HH_LOOKUP: [u8; 256] = [
    0, 1, 3, 4, 1, 2, 4, 5, 3, 4, 6, 7, 4, 5, 7, 7, 1, 2, 4, 5, 2, 2, 5, 5, 4,
    5, 7, 7, 5, 5, 7, 7, 3, 4, 6, 7, 4, 5, 7, 7, 6, 7, 8, 8, 7, 7, 8, 8, 4, 5,
    7, 7, 5, 5, 7, 7, 7, 7, 8, 8, 7, 7, 8, 8, 1, 2, 4, 5, 2, 2, 5, 5, 4, 5, 7,
    7, 5, 5, 7, 7, 2, 2, 5, 5, 2, 2, 5, 5, 5, 5, 7, 7, 5, 5, 7, 7, 4, 5, 7, 7,
    5, 5, 7, 7, 7, 7, 8, 8, 7, 7, 8, 8, 5, 5, 7, 7, 5, 5, 7, 7, 7, 7, 8, 8, 7,
    7, 8, 8, 3, 4, 6, 7, 4, 5, 7, 7, 6, 7, 8, 8, 7, 7, 8, 8, 4, 5, 7, 7, 5, 5,
    7, 7, 7, 7, 8, 8, 7, 7, 8, 8, 6, 7, 8, 8, 7, 7, 8, 8, 8, 8, 8, 8, 8, 8, 8,
    8, 7, 7, 8, 8, 7, 7, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 4, 5, 7, 7, 5, 5, 7, 7,
    7, 7, 8, 8, 7, 7, 8, 8, 5, 5, 7, 7, 5, 5, 7, 7, 7, 7, 8, 8, 7, 7, 8, 8, 7,
    7, 8, 8, 7, 7, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 7, 7, 8, 8, 7, 7, 8, 8, 8, 8,
    8, 8, 8, 8, 8, 8,
];

#[inline(always)]
fn decode_sign_bit<T: BitDecoder>(
    pos: Position,
    ctx: &mut BitPlaneDecodeContext,
    decoder: &mut T,
) -> Option<()> {
    #[inline(always)]
    fn context_label_sign_coding(pos: Position, ctx: &BitPlaneDecodeContext) -> (u8, u8) {
        let significances = ctx.neighborhood_significance_states(pos) & 0b0101_0101;

        let left_sign = ctx.sign(pos.left());
        let right_sign = ctx.sign(pos.right());
        let top_sign = ctx.sign(pos.top());
        let bottom_sign = if ctx.style.vertically_causal_context && ctx.neighbor_in_next_stripe(pos)
        {
            0
        } else {
            ctx.sign(pos.bottom())
        };

        let signs = (top_sign << 6) | (left_sign << 4) | (right_sign << 2) | bottom_sign;
        let negative_significances = significances & signs;
        let positive_significances = significances & !signs;
        let merged_significances = (negative_significances << 1) | positive_significances;

        SIGN_CONTEXT_LOOKUP
            .get(merged_significances as usize)
            .copied()
            .unwrap_or((0, 0))
    }

    let (ctx_label, xor_bit) = context_label_sign_coding(pos, ctx);
    let ad_ctx = ctx.arithmetic_decoder_context(ctx_label)?;
    let sign_bit = if T::IS_BYPASS {
        decoder.read_bit(ad_ctx)?
    } else {
        decoder.read_bit(ad_ctx)? ^ xor_bit as u32
    };
    ctx.set_sign(pos, sign_bit as u8);

    Some(())
}

#[inline(always)]
fn context_label_zero_coding(pos: Position, ctx: &BitPlaneDecodeContext) -> u8 {
    let neighbors = ctx.neighborhood_significance_states(pos) as usize;

    match ctx.sub_band_type {
        SubBandType::LowLow | SubBandType::LowHigh => {
            ZERO_CTX_LL_LH_LOOKUP.get(neighbors).copied().unwrap_or(0)
        }
        SubBandType::HighLow => ZERO_CTX_HL_LOOKUP.get(neighbors).copied().unwrap_or(0),
        SubBandType::HighHigh => ZERO_CTX_HH_LOOKUP.get(neighbors).copied().unwrap_or(0),
    }
}

fn context_label_magnitude_refinement_coding(pos: Position, ctx: &BitPlaneDecodeContext) -> u8 {
    let m1 = ctx.magnitude_refinement(pos) * 16;
    let m2 = 14 + ctx.neighborhood_significance_states(pos).min(1);

    u8::max(m1, m2)
}

#[derive(Default, Copy, Clone, Debug)]
struct Position {
    index_x: u32,
    index_y: u32,
}

impl Position {
    fn new(x: u32, y: u32) -> Self {
        Self {
            index_x: x + COEFFICIENTS_PADDING,
            index_y: y + COEFFICIENTS_PADDING,
        }
    }

    fn new_index(x: u32, y: u32) -> Self {
        Self {
            index_x: x,
            index_y: y,
        }
    }

    fn left(&self) -> Self {
        Self::new_index(self.index_x - 1, self.index_y)
    }

    fn right(&self) -> Self {
        Self::new_index(self.index_x + 1, self.index_y)
    }

    fn top(&self) -> Self {
        Self::new_index(self.index_x, self.index_y - 1)
    }

    fn bottom(&self) -> Self {
        Self::new_index(self.index_x, self.index_y + 1)
    }

    fn top_left(&self) -> Self {
        Self::new_index(self.index_x - 1, self.index_y - 1)
    }

    fn top_right(&self) -> Self {
        Self::new_index(self.index_x + 1, self.index_y - 1)
    }

    fn bottom_left(&self) -> Self {
        Self::new_index(self.index_x - 1, self.index_y + 1)
    }

    fn bottom_right(&self) -> Self {
        Self::new_index(self.index_x + 1, self.index_y + 1)
    }

    fn real_y(&self) -> u32 {
        self.index_y - 1
    }

    fn index(&self, padded_width: u32) -> usize {
        self.index_x as usize + self.index_y as usize * padded_width as usize
    }
}

trait BitDecoder {
    const IS_BYPASS: bool;

    fn read_bit(&mut self, context: &mut ArithmeticDecoderContext) -> Option<u32>;
}

impl BitDecoder for ArithmeticDecoder<'_> {
    const IS_BYPASS: bool = false;

    #[inline(always)]
    fn read_bit(&mut self, context: &mut ArithmeticDecoderContext) -> Option<u32> {
        Some(Self::read_bit(self, context))
    }
}

struct BypassDecoder<'a>(BitReader<'a>, bool);

impl<'a> BypassDecoder<'a> {
    fn new(data: &'a [u8], strict: bool) -> Self {
        Self(BitReader::new(data), strict)
    }
}

impl BitDecoder for BypassDecoder<'_> {
    const IS_BYPASS: bool = true;

    fn read_bit(&mut self, _: &mut ArithmeticDecoderContext) -> Option<u32> {
        self.0
            .read_bits_with_stuffing(1)
            .ok()
            .or(if !self.1 { Some(1) } else { None })
    }
}

mod fast_path {
    use super::{
        ArithmeticDecoder, ArithmeticDecoderContext, BitPlaneDecodeContext, COEFFICIENTS_PADDING,
        DecodeError, Result, SIGN_CONTEXT_LOOKUP, SubBandType, ZERO_CTX_HH_LOOKUP,
        ZERO_CTX_HL_LOOKUP, ZERO_CTX_LL_LH_LOOKUP, reset_context_array,
    };

    #[expect(clippy::indexing_slicing)]
    pub(super) fn write_reconstructed<M: Copy>(
        stripe_flags: &[u32],
        stripe_mags: &[[M; 4]],
        dst: &mut [f32],
        mut base_idx: usize,
        sb_width: usize,
        width: usize,
        height: usize,
        stride: usize,
        last_bit_position: u8,
        ended_on_spp: bool,
        irreversible: bool,
        roi_shift: u8,
        dequantization_step: f32,
        to_u32: impl Fn(M) -> u32,
    ) -> Result<()> {
        let pad = COEFFICIENTS_PADDING as usize;
        let stripe_count = height.div_ceil(4);
        let offset_main = if irreversible || last_bit_position != 0 {
            0.5 * (1_u32 << last_bit_position) as f32
        } else {
            0.0
        };
        let offset_prev = if ended_on_spp {
            0.5 * (1_u32 << (last_bit_position + 1)) as f32
        } else {
            offset_main
        };

        for stripe in 0..stripe_count {
            let flag_start = (stripe + pad) * stride + pad;
            let flag_row = stripe_flags
                .get(flag_start..)
                .and_then(|s| s.get(..width))
                .ok_or(DecodeError::Malformed)?;
            let mag_start = stripe * width;
            let mag_row = stripe_mags
                .get(mag_start..)
                .and_then(|s| s.get(..width))
                .ok_or(DecodeError::Malformed)?;
            let rows = (height - stripe * 4).min(4);

            for r in 0..rows {
                let row_start = base_idx + r * sb_width;
                let out_row = dst
                    .get_mut(row_start..)
                    .and_then(|s| s.get_mut(..width))
                    .ok_or(DecodeError::Malformed)?;
                out_row.fill(0.0);
            }

            if roi_shift == 0 {
                for (x, &flags) in flag_row.iter().enumerate() {
                    if flags & FLAG_SIGNIFICANT_ALL == 0 {
                        continue;
                    }
                    let mags = mag_row[x];
                    for r in 0..rows {
                        let shift = r as u32 * FLAGS_PER_COEFFICIENT;
                        if flags & (FLAG_SIGNIFICANT_THIS << shift) == 0 {
                            continue;
                        }
                        let sign = (flags >> (FLAG_SIGN_THIS_SHIFT + shift)) & 1;
                        let offset = if ended_on_spp && flags & (FLAG_ZERO_CODED_THIS << shift) == 0
                        {
                            offset_prev
                        } else {
                            offset_main
                        };
                        let mag = to_u32(mags[r]) as i32;
                        let signed = if sign == 0 { mag } else { -mag };
                        let val = if offset != 0.0 {
                            signed as f32 + if sign == 0 { offset } else { -offset }
                        } else {
                            signed as f32
                        };
                        dst[base_idx + r * sb_width + x] = val * dequantization_step;
                    }
                }
            } else {
                for (x, &flags) in flag_row.iter().enumerate() {
                    if flags & FLAG_SIGNIFICANT_ALL == 0 {
                        continue;
                    }
                    let mags = mag_row[x];
                    for r in 0..rows {
                        let shift = r as u32 * FLAGS_PER_COEFFICIENT;
                        if flags & (FLAG_SIGNIFICANT_THIS << shift) == 0 {
                            continue;
                        }
                        let mut mag = to_u32(mags[r]);
                        let sign = (flags >> (FLAG_SIGN_THIS_SHIFT + shift)) & 1;
                        let mut bit_position =
                            if ended_on_spp && flags & (FLAG_ZERO_CODED_THIS << shift) == 0 {
                                last_bit_position + 1
                            } else {
                                last_bit_position
                            };
                        let roi_magnitude = mag.checked_shr(u32::from(roi_shift)).unwrap_or(0);
                        if roi_magnitude != 0 {
                            mag = roi_magnitude;
                            bit_position = bit_position.saturating_sub(roi_shift);
                        }
                        let signed = if sign == 0 { mag as i32 } else { -(mag as i32) };
                        let val = if signed != 0 && (irreversible || bit_position != 0) {
                            let offset = 0.5 * (1_u32 << bit_position) as f32;
                            signed as f32 + if signed > 0 { offset } else { -offset }
                        } else {
                            signed as f32
                        };
                        dst[base_idx + r * sb_width + x] = val * dequantization_step;
                    }
                }
            }

            base_idx += rows * sb_width;
        }

        Ok(())
    }

    trait Magnitude: Copy {
        fn initial(bit_position: u8) -> Self;
        fn set_bit(&mut self, bit_position: u8);
    }

    impl Magnitude for u16 {
        #[inline(always)]
        fn initial(bit_position: u8) -> Self {
            1_u16 << bit_position
        }

        #[inline(always)]
        fn set_bit(&mut self, bit_position: u8) {
            *self |= 1_u16 << bit_position;
        }
    }

    impl Magnitude for u32 {
        #[inline(always)]
        fn initial(bit_position: u8) -> Self {
            1_u32 << bit_position
        }

        #[inline(always)]
        fn set_bit(&mut self, bit_position: u8) {
            *self |= 1_u32 << bit_position;
        }
    }

    struct FastState<'a, M> {
        stripe_flags: &'a mut [u32],
        stripe_mags: &'a mut [[M; 4]],
        contexts: &'a mut [ArithmeticDecoderContext; 32],
        zero_contexts: &'static [u8; 512],
        width: usize,
        stride: usize,
        full_stripes: usize,
        tail_rows: u32,
        current_bit_position: u8,
    }

    pub(super) fn handle_coding_passes(
        start: u8,
        end: u8,
        ctx: &mut BitPlaneDecodeContext,
        decoder: &mut ArithmeticDecoder<'_>,
    ) -> Option<()> {
        let reset_context_probabilities = ctx.style.reset_context_probabilities;
        let segmentation_symbols = ctx.style.segmentation_symbols;
        let bitplanes = ctx.bitplanes;
        let strict = ctx.strict;
        let zero_contexts = zero_context_lookup(ctx.sub_band_type);
        let width = ctx.width as usize;
        let stride = ctx.padded_width as usize;
        let full_stripes = (ctx.height / 4) as usize;
        let tail_rows = ctx.height % 4;
        let stripe_flags = ctx.stripe_flags.get_mut(..ctx.active_stripe_flags)?;

        if ctx.use_u16_mags {
            let mut state = FastState {
                stripe_flags,
                stripe_mags: ctx.stripe_mags_u16.get_mut(..ctx.active_stripe_mags)?,
                contexts: &mut ctx.contexts,
                zero_contexts,
                width,
                stride,
                full_stripes,
                tail_rows,
                current_bit_position: 0,
            };
            run_coding_passes(
                start,
                end,
                bitplanes,
                reset_context_probabilities,
                segmentation_symbols,
                strict,
                &mut state,
                decoder,
            )?;
        } else {
            let mut state = FastState {
                stripe_flags,
                stripe_mags: ctx.stripe_mags_u32.get_mut(..ctx.active_stripe_mags)?,
                contexts: &mut ctx.contexts,
                zero_contexts,
                width,
                stride,
                full_stripes,
                tail_rows,
                current_bit_position: 0,
            };
            run_coding_passes(
                start,
                end,
                bitplanes,
                reset_context_probabilities,
                segmentation_symbols,
                strict,
                &mut state,
                decoder,
            )?;
        }

        let last_pass = end - 1;
        ctx.last_bit_position = bitplanes - 1 - last_pass.div_ceil(3);
        ctx.ended_on_spp = end % 3 == 2;

        Some(())
    }

    fn run_coding_passes<M: Magnitude>(
        start: u8,
        end: u8,
        bitplanes: u8,
        reset_context_probabilities: bool,
        segmentation_symbols: bool,
        strict: bool,
        state: &mut FastState<'_, M>,
        decoder: &mut ArithmeticDecoder<'_>,
    ) -> Option<()> {
        for coding_pass in start..end {
            let current_bitplane = coding_pass.div_ceil(3);
            state.current_bit_position = bitplanes - 1 - current_bitplane;

            match coding_pass % 3 {
                0 => {
                    cleanup_pass(state, decoder);

                    if segmentation_symbols {
                        let b0 = decoder.read_bit(&mut state.contexts[18]);
                        let b1 = decoder.read_bit(&mut state.contexts[18]);
                        let b2 = decoder.read_bit(&mut state.contexts[18]);
                        let b3 = decoder.read_bit(&mut state.contexts[18]);

                        if (b0 != 1 || b1 != 0 || b2 != 1 || b3 != 0) && strict {
                            return None;
                        }
                    }
                }
                1 => significance_propagation_pass(state, decoder),
                2 => magnitude_refinement_pass(state, decoder),
                _ => unreachable!(),
            }

            if reset_context_probabilities {
                reset_context_array(state.contexts);
            }
        }

        Some(())
    }

    #[expect(clippy::indexing_slicing)]
    fn cleanup_pass<M: Magnitude>(
        state: &mut FastState<'_, M>,
        decoder: &mut ArithmeticDecoder<'_>,
    ) {
        let mut dec = *decoder;
        let width = state.width;
        let stride = state.stride;
        let full_stripes = state.full_stripes;
        let tail_rows = state.tail_rows;
        let padding = COEFFICIENTS_PADDING as usize;
        let mut flag_base = padding * stride + padding;
        let mut mag_base = 0;

        for _ in 0..full_stripes {
            let mut x = 0;
            while x < width {
                let flag_idx = flag_base + x;
                let orig_flags = state.stripe_flags[flag_idx];

                if orig_flags == 0 {
                    let mut rl_ctx = state.contexts[17];
                    let (next_x, hit_one) = dec.skip_zero_run_columns(
                        &mut rl_ctx,
                        &state.stripe_flags[flag_base..flag_base + width],
                        x,
                    );
                    state.contexts[17] = rl_ctx;
                    x = next_x;
                    if hit_one {
                        let mut num_zeroes = dec.read_bit(&mut state.contexts[18]);
                        num_zeroes = (num_zeroes << 1) | dec.read_bit(&mut state.contexts[18]);
                        let mut flags = 0;
                        let hit_flag_idx = flag_base + x;
                        let column = StripeColumn {
                            flag_idx: hit_flag_idx,
                            mag_idx: mag_base + x,
                            stride,
                        };
                        cleanup_from_run_length(state, &mut dec, column, &mut flags, num_zeroes);
                        state.stripe_flags[hit_flag_idx] = flags;
                        x += 1;
                    }
                    continue;
                }

                let mut flags = orig_flags;
                if (flags & FLAG_SIGNIFICANT_ALL) == FLAG_SIGNIFICANT_ALL {
                    if (flags & FLAG_ZERO_CODED_ALL) != 0 {
                        state.stripe_flags[flag_idx] = flags & !FLAG_ZERO_CODED_ALL;
                    }
                } else {
                    let column = StripeColumn {
                        flag_idx,
                        mag_idx: mag_base + x,
                        stride,
                    };
                    cleanup_coefficient(state, &mut dec, column, &mut flags, 0, true, false);
                    cleanup_coefficient(state, &mut dec, column, &mut flags, 1, true, false);
                    cleanup_coefficient(state, &mut dec, column, &mut flags, 2, true, false);
                    cleanup_coefficient(state, &mut dec, column, &mut flags, 3, true, false);

                    let next = flags & !FLAG_ZERO_CODED_ALL;
                    if next != orig_flags {
                        state.stripe_flags[flag_idx] = next;
                    }
                }
                x += 1;
            }

            flag_base += stride;
            mag_base += width;
        }

        if tail_rows > 0 {
            for x in 0..width {
                let flag_idx = flag_base + x;
                let orig_flags = state.stripe_flags[flag_idx];
                let mut flags = orig_flags;
                let column = StripeColumn {
                    flag_idx,
                    mag_idx: mag_base + x,
                    stride,
                };
                for r in 0..tail_rows {
                    cleanup_coefficient(state, &mut dec, column, &mut flags, r, true, false);
                }

                let next = flags & !FLAG_ZERO_CODED_ALL;
                if next != orig_flags {
                    state.stripe_flags[flag_idx] = next;
                }
            }
        }

        *decoder = dec;
    }

    #[inline(always)]
    fn cleanup_from_run_length<M: Magnitude>(
        state: &mut FastState<'_, M>,
        decoder: &mut ArithmeticDecoder<'_>,
        column: StripeColumn,
        flags: &mut u32,
        num_zeroes: u32,
    ) {
        macro_rules! decode {
            ($coefficient_in_stripe:expr) => {
                cleanup_coefficient(
                    state,
                    decoder,
                    column,
                    flags,
                    $coefficient_in_stripe,
                    false,
                    false,
                )
            };
        }

        macro_rules! significant {
            ($coefficient_in_stripe:expr) => {
                cleanup_coefficient(
                    state,
                    decoder,
                    column,
                    flags,
                    $coefficient_in_stripe,
                    false,
                    true,
                )
            };
        }

        match num_zeroes {
            0 => {
                significant!(0);
                decode!(1);
                decode!(2);
                decode!(3);
            }
            1 => {
                significant!(1);
                decode!(2);
                decode!(3);
            }
            2 => {
                significant!(2);
                decode!(3);
            }
            _ => significant!(3),
        }
    }

    #[inline(always)]
    fn cleanup_coefficient<M: Magnitude>(
        state: &mut FastState<'_, M>,
        decoder: &mut ArithmeticDecoder<'_>,
        column: StripeColumn,
        flags: &mut u32,
        coefficient_in_stripe: u32,
        check_flags: bool,
        already_known_significant: bool,
    ) {
        debug_assert!(coefficient_in_stripe < 4);
        let shift = coefficient_in_stripe * FLAGS_PER_COEFFICIENT;

        if !check_flags || (*flags & ((FLAG_SIGNIFICANT_THIS | FLAG_ZERO_CODED_THIS) << shift)) == 0
        {
            if !already_known_significant {
                #[expect(clippy::indexing_slicing)]
                let ctx_label =
                    state.zero_contexts[((*flags >> shift) & FLAG_SIGNIFICANT_NEIGHBORS) as usize];
                #[expect(clippy::indexing_slicing)]
                let ad_ctx = &mut state.contexts[(ctx_label & 0x1F) as usize];
                if decoder.read_bit(ad_ctx) == 0 {
                    return;
                }
            }

            let sign = decode_sign_bit(state, decoder, column, *flags, coefficient_in_stripe);
            set_initial_significant(state, column.mag_idx, coefficient_in_stripe as usize);
            set_significant(state, column, flags, coefficient_in_stripe, sign);
        }
    }

    #[expect(clippy::indexing_slicing)]
    fn significance_propagation_pass<M: Magnitude>(
        state: &mut FastState<'_, M>,
        decoder: &mut ArithmeticDecoder<'_>,
    ) {
        let mut dec = *decoder;
        let width = state.width;
        let stride = state.stride;
        let full_stripes = state.full_stripes;
        let tail_rows = state.tail_rows;
        let padding = COEFFICIENTS_PADDING as usize;
        let mut flag_base = padding * stride + padding;
        let mut mag_base = 0;

        for _ in 0..full_stripes {
            for x in 0..width {
                let flag_idx = flag_base + x;
                let orig_flags = state.stripe_flags[flag_idx];
                if orig_flags == 0 || (orig_flags & FLAG_SIGNIFICANT_ALL) == FLAG_SIGNIFICANT_ALL {
                    continue;
                }

                let mut flags = orig_flags;
                let column = StripeColumn {
                    flag_idx,
                    mag_idx: mag_base + x,
                    stride,
                };
                significance_propagation_coefficient(state, &mut dec, column, &mut flags, 0);
                significance_propagation_coefficient(state, &mut dec, column, &mut flags, 1);
                significance_propagation_coefficient(state, &mut dec, column, &mut flags, 2);
                significance_propagation_coefficient(state, &mut dec, column, &mut flags, 3);

                if flags != orig_flags {
                    state.stripe_flags[flag_idx] = flags;
                }
            }

            flag_base += stride;
            mag_base += width;
        }

        if tail_rows > 0 {
            for x in 0..width {
                let flag_idx = flag_base + x;
                let orig_flags = state.stripe_flags[flag_idx];
                if orig_flags == 0 {
                    continue;
                }

                let mut flags = orig_flags;
                let column = StripeColumn {
                    flag_idx,
                    mag_idx: mag_base + x,
                    stride,
                };
                for r in 0..tail_rows {
                    significance_propagation_coefficient(state, &mut dec, column, &mut flags, r);
                }

                if flags != orig_flags {
                    state.stripe_flags[flag_idx] = flags;
                }
            }
        }

        *decoder = dec;
    }

    #[inline(always)]
    fn significance_propagation_coefficient<M: Magnitude>(
        state: &mut FastState<'_, M>,
        decoder: &mut ArithmeticDecoder<'_>,
        column: StripeColumn,
        flags: &mut u32,
        coefficient_in_stripe: u32,
    ) {
        debug_assert!(coefficient_in_stripe < 4);
        let shift = coefficient_in_stripe * FLAGS_PER_COEFFICIENT;
        let shifted_flags = *flags >> shift;
        let should_propagate = shifted_flags & (FLAG_SIGNIFICANT_THIS | FLAG_ZERO_CODED_THIS) == 0
            && shifted_flags & FLAG_SIGNIFICANT_NEIGHBORS != 0;

        if should_propagate {
            #[expect(clippy::indexing_slicing)]
            let ctx_label =
                state.zero_contexts[(shifted_flags & FLAG_SIGNIFICANT_NEIGHBORS) as usize];
            #[expect(clippy::indexing_slicing)]
            let bit = decoder.read_bit(&mut state.contexts[(ctx_label & 0x1F) as usize]);
            *flags |= FLAG_ZERO_CODED_THIS << shift;

            if bit == 1 {
                let sign = decode_sign_bit(state, decoder, column, *flags, coefficient_in_stripe);
                set_initial_significant(state, column.mag_idx, coefficient_in_stripe as usize);
                set_significant(state, column, flags, coefficient_in_stripe, sign);
            }
        }
    }

    #[expect(clippy::indexing_slicing)]
    fn magnitude_refinement_pass<M: Magnitude>(
        state: &mut FastState<'_, M>,
        decoder: &mut ArithmeticDecoder<'_>,
    ) {
        let mut dec = *decoder;
        let width = state.width;
        let stride = state.stride;
        let full_stripes = state.full_stripes;
        let tail_rows = state.tail_rows;
        let padding = COEFFICIENTS_PADDING as usize;
        let mut flag_base = padding * stride + padding;
        let mut mag_base = 0;

        for _ in 0..full_stripes {
            for x in 0..width {
                let flag_idx = flag_base + x;
                let orig_flags = state.stripe_flags[flag_idx];
                if (orig_flags & FLAG_SIGNIFICANT_ALL) == 0 {
                    continue;
                }

                let mut flags = orig_flags;
                let column = StripeColumn {
                    flag_idx,
                    mag_idx: mag_base + x,
                    stride,
                };
                magnitude_refinement_coefficient(state, &mut dec, column, &mut flags, 0);
                magnitude_refinement_coefficient(state, &mut dec, column, &mut flags, 1);
                magnitude_refinement_coefficient(state, &mut dec, column, &mut flags, 2);
                magnitude_refinement_coefficient(state, &mut dec, column, &mut flags, 3);

                if flags != orig_flags {
                    state.stripe_flags[flag_idx] = flags;
                }
            }

            flag_base += stride;
            mag_base += width;
        }

        if tail_rows > 0 {
            for x in 0..width {
                let flag_idx = flag_base + x;
                let orig_flags = state.stripe_flags[flag_idx];
                if (orig_flags & FLAG_SIGNIFICANT_ALL) == 0 {
                    continue;
                }

                let mut flags = orig_flags;
                let column = StripeColumn {
                    flag_idx,
                    mag_idx: mag_base + x,
                    stride,
                };
                for r in 0..tail_rows {
                    magnitude_refinement_coefficient(state, &mut dec, column, &mut flags, r);
                }

                if flags != orig_flags {
                    state.stripe_flags[flag_idx] = flags;
                }
            }
        }

        *decoder = dec;
    }

    #[inline(always)]
    fn magnitude_refinement_coefficient<M: Magnitude>(
        state: &mut FastState<'_, M>,
        decoder: &mut ArithmeticDecoder<'_>,
        column: StripeColumn,
        flags: &mut u32,
        coefficient_in_stripe: u32,
    ) {
        let shift = coefficient_in_stripe * FLAGS_PER_COEFFICIENT;
        let shifted_flags = *flags >> shift;
        let should_decode_refinement =
            shifted_flags & (FLAG_SIGNIFICANT_THIS | FLAG_ZERO_CODED_THIS) == FLAG_SIGNIFICANT_THIS;

        if should_decode_refinement {
            let ctx_label = magnitude_refinement_context(shifted_flags);
            #[expect(clippy::indexing_slicing)]
            let bit = decoder.read_bit(&mut state.contexts[(ctx_label & 0x1F) as usize]);
            if bit == 1 {
                push_magnitude_bit(state, column.mag_idx, coefficient_in_stripe as usize);
            }
            *flags |= FLAG_MAGNITUDE_REFINED_THIS << shift;
        }
    }

    #[inline(always)]
    #[expect(clippy::indexing_slicing)]
    fn decode_sign_bit<M>(
        state: &mut FastState<'_, M>,
        decoder: &mut ArithmeticDecoder<'_>,
        column: StripeColumn,
        flags: u32,
        coefficient_in_stripe: u32,
    ) -> u32 {
        let shift = coefficient_in_stripe * FLAGS_PER_COEFFICIENT;
        let shifted_flags = flags >> shift;
        let left_flags = state.stripe_flags[column.flag_idx - 1];
        let right_flags = state.stripe_flags[column.flag_idx + 1];

        let top_sign = if coefficient_in_stripe == 0 {
            (flags >> FLAG_SIGN_TOP_STRIPE_SHIFT) & 1
        } else {
            (flags >> (FLAG_SIGN_THIS_SHIFT + (coefficient_in_stripe - 1) * 3)) & 1
        };
        let bottom_sign = (flags
            >> (FLAG_SIGN_BOTTOM_THIS_SHIFT + coefficient_in_stripe * FLAGS_PER_COEFFICIENT))
            & 1;
        let left_sign = (left_flags >> (FLAG_SIGN_THIS_SHIFT + shift)) & 1;
        let right_sign = (right_flags >> (FLAG_SIGN_THIS_SHIFT + shift)) & 1;

        let lookup = left_sign
            | (((shifted_flags & FLAG_SIGNIFICANT_TOP) != 0) as u32) << 1
            | (right_sign << 2)
            | (((shifted_flags & FLAG_SIGNIFICANT_LEFT) != 0) as u32) << 3
            | (top_sign << 4)
            | (((shifted_flags & FLAG_SIGNIFICANT_RIGHT) != 0) as u32) << 5
            | (bottom_sign << 6)
            | (((shifted_flags & FLAG_SIGNIFICANT_BOTTOM) != 0) as u32) << 7;

        let (ctx_label, xor_bit) = SIGN_CONTEXT_STRIPE_LOOKUP[lookup as usize];
        decoder.read_bit(&mut state.contexts[(ctx_label & 0x1F) as usize]) ^ xor_bit as u32
    }

    #[inline(always)]
    fn zero_context_lookup(sub_band_type: SubBandType) -> &'static [u8; 512] {
        match sub_band_type {
            SubBandType::LowLow | SubBandType::LowHigh => &ZERO_CTX_LL_LH_STRIPE_LOOKUP,
            SubBandType::HighLow => &ZERO_CTX_HL_STRIPE_LOOKUP,
            SubBandType::HighHigh => &ZERO_CTX_HH_STRIPE_LOOKUP,
        }
    }

    #[inline(always)]
    fn magnitude_refinement_context(flags: u32) -> u8 {
        if flags & FLAG_MAGNITUDE_REFINED_THIS != 0 {
            16
        } else {
            14 + ((flags & FLAG_SIGNIFICANT_NEIGHBORS != 0) as u8)
        }
    }

    #[derive(Copy, Clone)]
    struct StripeColumn {
        flag_idx: usize,
        mag_idx: usize,
        stride: usize,
    }

    #[inline(always)]
    #[expect(clippy::indexing_slicing)]
    fn push_magnitude_bit<M: Magnitude>(state: &mut FastState<'_, M>, mag_idx: usize, row: usize) {
        state.stripe_mags[mag_idx][row].set_bit(state.current_bit_position);
    }

    #[inline(always)]
    #[expect(clippy::indexing_slicing)]
    fn set_initial_significant<M: Magnitude>(
        state: &mut FastState<'_, M>,
        mag_idx: usize,
        row: usize,
    ) {
        state.stripe_mags[mag_idx][row] = M::initial(state.current_bit_position);
    }

    #[inline(always)]
    #[expect(clippy::indexing_slicing)]
    fn set_significant<M>(
        state: &mut FastState<'_, M>,
        column: StripeColumn,
        flags: &mut u32,
        coefficient_in_stripe: u32,
        sign: u32,
    ) {
        let shift = coefficient_in_stripe * FLAGS_PER_COEFFICIENT;

        state.stripe_flags[column.flag_idx - 1] |= FLAG_SIGNIFICANT_RIGHT << shift;
        *flags |= ((sign << FLAG_SIGN_THIS_SHIFT) | FLAG_SIGNIFICANT_THIS) << shift;
        state.stripe_flags[column.flag_idx + 1] |= FLAG_SIGNIFICANT_LEFT << shift;

        if coefficient_in_stripe == 0 {
            let top = column.flag_idx - column.stride;
            state.stripe_flags[top] |= (sign << FLAG_SIGN_BOTTOM_STRIPE_SHIFT)
                | (FLAG_SIGNIFICANT_BOTTOM << LAST_COEFFICIENT_SHIFT);
            state.stripe_flags[top - 1] |= FLAG_SIGNIFICANT_BOTTOM_RIGHT << LAST_COEFFICIENT_SHIFT;
            state.stripe_flags[top + 1] |= FLAG_SIGNIFICANT_BOTTOM_LEFT << LAST_COEFFICIENT_SHIFT;
        }

        if coefficient_in_stripe == 3 {
            let bottom = column.flag_idx + column.stride;
            state.stripe_flags[bottom] |=
                (sign << FLAG_SIGN_TOP_STRIPE_SHIFT) | FLAG_SIGNIFICANT_TOP;
            state.stripe_flags[bottom - 1] |= FLAG_SIGNIFICANT_TOP_RIGHT;
            state.stripe_flags[bottom + 1] |= FLAG_SIGNIFICANT_TOP_LEFT;
        }
    }

    const FLAGS_PER_COEFFICIENT: u32 = 3;
    const LAST_COEFFICIENT_SHIFT: u32 = 3 * FLAGS_PER_COEFFICIENT;

    const FLAG_SIGNIFICANT_TOP_LEFT: u32 = 1 << 0;
    const FLAG_SIGNIFICANT_TOP: u32 = 1 << 1;
    const FLAG_SIGNIFICANT_TOP_RIGHT: u32 = 1 << 2;
    const FLAG_SIGNIFICANT_LEFT: u32 = 1 << 3;
    const FLAG_SIGNIFICANT_THIS: u32 = 1 << 4;
    const FLAG_SIGNIFICANT_RIGHT: u32 = 1 << 5;
    const FLAG_SIGNIFICANT_BOTTOM_LEFT: u32 = 1 << 6;
    const FLAG_SIGNIFICANT_BOTTOM: u32 = 1 << 7;
    const FLAG_SIGNIFICANT_BOTTOM_RIGHT: u32 = 1 << 8;
    const FLAG_SIGNIFICANT_ALL: u32 = FLAG_SIGNIFICANT_THIS
        | (FLAG_SIGNIFICANT_THIS << FLAGS_PER_COEFFICIENT)
        | (FLAG_SIGNIFICANT_THIS << (2 * FLAGS_PER_COEFFICIENT))
        | (FLAG_SIGNIFICANT_THIS << LAST_COEFFICIENT_SHIFT);
    const FLAG_SIGNIFICANT_NEIGHBORS: u32 = FLAG_SIGNIFICANT_TOP_LEFT
        | FLAG_SIGNIFICANT_TOP
        | FLAG_SIGNIFICANT_TOP_RIGHT
        | FLAG_SIGNIFICANT_LEFT
        | FLAG_SIGNIFICANT_RIGHT
        | FLAG_SIGNIFICANT_BOTTOM_LEFT
        | FLAG_SIGNIFICANT_BOTTOM
        | FLAG_SIGNIFICANT_BOTTOM_RIGHT;

    const FLAG_SIGN_TOP_STRIPE_SHIFT: u32 = 18;
    const FLAG_SIGN_THIS_SHIFT: u32 = 19;
    const FLAG_MAGNITUDE_REFINED_THIS: u32 = 1 << 20;
    const FLAG_ZERO_CODED_THIS: u32 = 1 << 21;
    const FLAG_SIGN_BOTTOM_THIS_SHIFT: u32 = 22;
    const FLAG_SIGN_BOTTOM_STRIPE_SHIFT: u32 = 31;
    const FLAG_ZERO_CODED_ALL: u32 = (1 << 21) | (1 << 24) | (1 << 27) | (1 << 30);

    const SIGN_CONTEXT_STRIPE_LOOKUP: [(u8, u8); 256] = build_sign_context_lookup();

    #[expect(clippy::indexing_slicing)]
    const fn build_sign_context_lookup() -> [(u8, u8); 256] {
        let mut out = [(0, 0); 256];
        let mut lookup = 0;

        while lookup < 256 {
            let left_sign = lookup & 1;
            let top_significance = (lookup >> 1) & 1;
            let right_sign = (lookup >> 2) & 1;
            let left_significance = (lookup >> 3) & 1;
            let top_sign = (lookup >> 4) & 1;
            let right_significance = (lookup >> 5) & 1;
            let bottom_sign = (lookup >> 6) & 1;
            let bottom_significance = (lookup >> 7) & 1;

            let significances = (top_significance << 6)
                | (left_significance << 4)
                | (right_significance << 2)
                | bottom_significance;
            let signs = (top_sign << 6) | (left_sign << 4) | (right_sign << 2) | bottom_sign;
            let merged_significances = ((significances & signs) << 1) | (significances & !signs);

            out[lookup] = SIGN_CONTEXT_LOOKUP[merged_significances];
            lookup += 1;
        }

        out
    }

    const ZERO_CTX_LL_LH_STRIPE_LOOKUP: [u8; 512] =
        build_zero_context_lookup(ZERO_CTX_LL_LH_LOOKUP);
    const ZERO_CTX_HL_STRIPE_LOOKUP: [u8; 512] = build_zero_context_lookup(ZERO_CTX_HL_LOOKUP);
    const ZERO_CTX_HH_STRIPE_LOOKUP: [u8; 512] = build_zero_context_lookup(ZERO_CTX_HH_LOOKUP);

    #[expect(clippy::indexing_slicing)]
    const fn build_zero_context_lookup(source: [u8; 256]) -> [u8; 512] {
        let mut out = [0; 512];
        let mut flags = 0;

        while flags < 512 {
            out[flags] = source[neighbor_byte(flags)];
            flags += 1;
        }

        out
    }

    const fn neighbor_byte(flags: usize) -> usize {
        ((((flags & FLAG_SIGNIFICANT_TOP_LEFT as usize) != 0) as usize) << 7)
            | ((((flags & FLAG_SIGNIFICANT_TOP as usize) != 0) as usize) << 6)
            | ((((flags & FLAG_SIGNIFICANT_TOP_RIGHT as usize) != 0) as usize) << 5)
            | ((((flags & FLAG_SIGNIFICANT_LEFT as usize) != 0) as usize) << 4)
            | ((((flags & FLAG_SIGNIFICANT_BOTTOM_LEFT as usize) != 0) as usize) << 3)
            | ((((flags & FLAG_SIGNIFICANT_RIGHT as usize) != 0) as usize) << 2)
            | ((((flags & FLAG_SIGNIFICANT_BOTTOM_RIGHT as usize) != 0) as usize) << 1)
            | (((flags & FLAG_SIGNIFICANT_BOTTOM as usize) != 0) as usize)
    }
}
