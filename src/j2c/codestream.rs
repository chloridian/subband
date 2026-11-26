use std::vec;
use std::vec::Vec;

use super::DecodeSettings;
use super::build::SubBandType;
use crate::error::{DecodeError, Result, bail};
use crate::reader::BitReader;

const MAX_TILES: u32 = 256;
const MAX_DECOMPOSITION_LEVELS: u8 = 12;
const MAX_LAYER_COUNT: u8 = 32;
const MAX_PRECINCT_EXPONENT: u8 = 31;
const MAX_CODE_BLOCKS: u64 = 65_536;
const MAX_CODE_BLOCK_LAYERS: u64 = 262_144;
const MAX_PRECINCT_LAYERS: u64 = 65_536;
const MAX_MARKERS: usize = 4_096;
const MAX_MARKER_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MarkerBudget {
    markers: usize,
    bytes: usize,
}

impl MarkerBudget {
    pub(crate) fn record(&mut self, segment_bytes: usize) -> Result<()> {
        self.markers += 1;
        self.bytes = self.bytes.saturating_add(segment_bytes);
        if self.markers > MAX_MARKERS || self.bytes > MAX_MARKER_BYTES {
            bail!(DecodeError::TooLarge);
        }

        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct Header {
    pub(crate) size_data: SizeData,
    pub(crate) global_coding_style: CodingStyleDefault,
    pub(crate) component_infos: Vec<ComponentInfo>,
    pub(crate) ppm_packets: Vec<PpmPacket>,
    pub(crate) progression_changes: Vec<ProgressionChange>,
    pub(crate) skipped_resolution_levels: u8,
    pub(crate) strict: bool,
    pub(crate) marker_budget: MarkerBudget,
}

#[derive(Debug, Clone)]
pub(crate) struct PpmMarkerData<'a> {
    pub(crate) sequence_idx: u8,
    pub(crate) data: &'a [u8],
}

pub(crate) type PpmPacket = Vec<u8>;

pub(crate) fn read_header<'a>(
    reader: &mut BitReader<'a>,
    settings: &DecodeSettings,
) -> Result<Header> {
    let mut marker_budget = MarkerBudget::default();
    marker_budget.record(0)?;

    if reader.read_marker()? != markers::SIZ {
        bail!(DecodeError::Malformed);
    }

    let siz_start = reader.offset();
    let mut size_data = size_marker(reader, settings)?;
    marker_budget.record(reader.offset() - siz_start)?;
    if size_data.num_tiles()? > MAX_TILES {
        bail!(DecodeError::TooLarge);
    }

    let mut cod = None;
    let mut qcd = None;

    let num_components = size_data.num_components;
    let mut cod_components = vec![None; num_components as usize];
    let mut qcd_components = vec![None; num_components as usize];
    let mut roi_shifts = vec![0; num_components as usize];
    let mut ppm_markers = vec![];
    let mut progression_changes = vec![];

    loop {
        let marker = reader.peek_marker().ok_or(DecodeError::Malformed)?;
        if marker == markers::SOT {
            break;
        }

        reader.read_marker()?;
        let seg_start = reader.offset();

        match marker {
            markers::COD => {
                cod = Some(cod_marker(reader)?);
            }
            markers::COC => {
                let (component_index, coc) = coc_marker(reader)?;
                *cod_components
                    .get_mut(component_index as usize)
                    .ok_or(DecodeError::Malformed)? = Some(coc);
            }
            markers::QCD => {
                qcd = Some(qcd_marker(reader).ok_or(DecodeError::Malformed)?);
            }
            markers::QCC => {
                let (component_index, qcc) = qcc_marker(reader).ok_or(DecodeError::Malformed)?;
                *qcd_components
                    .get_mut(component_index as usize)
                    .ok_or(DecodeError::Malformed)? = Some(qcc);
            }
            markers::RGN => {
                let (component_index, roi_shift) =
                    rgn_marker(reader).ok_or(DecodeError::Malformed)?;
                *roi_shifts
                    .get_mut(component_index as usize)
                    .ok_or(DecodeError::Malformed)? = roi_shift;
            }
            markers::TLM => {
                tlm_marker(reader).ok_or(DecodeError::Malformed)?;
            }
            markers::COM => {
                com_marker(reader).ok_or(DecodeError::Malformed)?;
            }
            markers::PPM => {
                ppm_markers.push(ppm_marker(reader).ok_or(DecodeError::Malformed)?);
            }
            markers::POC => {
                progression_changes.extend(poc_marker(reader).ok_or(DecodeError::Malformed)?);
            }
            markers::CRG => {
                skip_marker_segment(reader);
            }
            (0x30..=0x3F) => {}
            _ => {
                bail!(DecodeError::Unsupported);
            }
        }

        marker_budget.record(reader.offset() - seg_start)?;
    }

    let cod = cod.ok_or(DecodeError::Malformed)?;
    let qcd = qcd.ok_or(DecodeError::Malformed)?;

    let component_infos: Vec<ComponentInfo> = (0..num_components as usize)
        .map(|idx| ComponentInfo {
            coding_style: cod_components[idx]
                .clone()
                .map(|mut c| {
                    c.flags.raw |= cod.component_parameters.flags.raw;

                    c
                })
                .unwrap_or(cod.component_parameters.clone()),
            quantization_info: qcd_components[idx].clone().unwrap_or(qcd.clone()),
            roi_shift: roi_shifts[idx],
        })
        .collect();

    validate_resource_limits(&size_data, &component_infos, cod.num_layers)?;

    let min_num_resolution_levels = component_infos
        .iter()
        .map(|c| c.num_resolution_levels())
        .min()
        .unwrap();
    let skipped_resolution_levels =
        if let Some((target_width, target_height)) = settings.target_resolution {
            let width_log = size_data
                .image_width()
                .checked_div(target_width)
                .and_then(|v| v.checked_ilog2())
                .unwrap_or(0);
            let height_log = size_data
                .image_height()
                .checked_div(target_height)
                .and_then(|v| v.checked_ilog2())
                .unwrap_or(0);

            width_log.min(height_log) as u8
        } else {
            0
        }
        .min(min_num_resolution_levels - 1);

    size_data.resolution_shrink_factor *= 1 << skipped_resolution_levels;

    let output_bytes = u64::from(size_data.image_width())
        .saturating_mul(u64::from(size_data.image_height()))
        .saturating_mul(u64::from(size_data.num_components));
    if output_bytes > settings.max_output as u64 {
        bail!(DecodeError::TooLarge);
    }

    let ppm_packets = ppm_packets(ppm_markers).ok_or(DecodeError::Malformed)?;

    let header = Header {
        size_data,
        global_coding_style: cod.clone(),
        component_infos,
        ppm_packets,
        progression_changes,
        skipped_resolution_levels,
        strict: settings.strict,
        marker_budget,
    };

    validate(&header)?;

    Ok(header)
}

pub(crate) fn validate_resource_limits(
    size_data: &SizeData,
    component_infos: &[ComponentInfo],
    num_layers: u8,
) -> Result<()> {
    let layers = u64::from(num_layers);
    let mut total_code_blocks = 0_u64;
    let mut total_precincts = 0_u64;

    for info in component_infos {
        let params = &info.coding_style.parameters;
        let n_l = params.num_decomposition_levels;

        for r in 0..=n_l {
            let shift = u32::from(n_l - r);
            let w_r = u64::from(size_data.reference_grid_width).div_ceil(1_u64 << shift);
            let h_r = u64::from(size_data.reference_grid_height).div_ceil(1_u64 << shift);
            let (ppx, ppy) = params.precinct_exponents[r as usize];

            let precincts_r = w_r
                .div_ceil(1_u64 << ppx)
                .saturating_mul(h_r.div_ceil(1_u64 << ppy));
            total_precincts = total_precincts.saturating_add(precincts_r);

            if r == 0 {
                let cbw = 1_u64 << params.code_block_width.min(ppx);
                let cbh = 1_u64 << params.code_block_height.min(ppy);
                let cbs = (w_r.div_ceil(cbw) + 1).saturating_mul(h_r.div_ceil(cbh) + 1);
                total_code_blocks = total_code_blocks.saturating_add(cbs);
            } else {
                let cbw = 1_u64 << params.code_block_width.min(ppx.saturating_sub(1));
                let cbh = 1_u64 << params.code_block_height.min(ppy.saturating_sub(1));
                let bw = w_r.div_ceil(2);
                let bh = h_r.div_ceil(2);
                let cbs_per_band = (bw.div_ceil(cbw) + 1).saturating_mul(bh.div_ceil(cbh) + 1);
                total_code_blocks =
                    total_code_blocks.saturating_add(cbs_per_band.saturating_mul(3));
            }
        }
    }

    if total_code_blocks > MAX_CODE_BLOCKS
        || total_code_blocks.saturating_mul(layers) > MAX_CODE_BLOCK_LAYERS
        || total_precincts.saturating_mul(layers) > MAX_PRECINCT_LAYERS
    {
        bail!(DecodeError::TooLarge);
    }

    Ok(())
}

fn validate(header: &Header) -> Result<()> {
    for info in &header.component_infos {
        let max_resolution_idx = info.coding_style.parameters.num_resolution_levels - 1;
        let quantization_style = info.quantization_info.quantization_style;
        let num_precinct_exponents = info.quantization_info.step_sizes.len();

        if num_precinct_exponents == 0 {
            bail!(DecodeError::Malformed);
        } else if matches!(
            quantization_style,
            QuantizationStyle::NoQuantization | QuantizationStyle::ScalarExpounded
        ) {
            if max_resolution_idx == 0 {
                if num_precinct_exponents == 0 {
                    bail!(DecodeError::Malformed);
                }
            } else if 1 + (max_resolution_idx as usize - 1) * 3 + 2 >= num_precinct_exponents {
                bail!(DecodeError::Malformed);
            }
        }
    }

    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct ComponentInfo {
    pub(crate) coding_style: CodingStyleComponent,
    pub(crate) quantization_info: QuantizationInfo,
    pub(crate) roi_shift: u8,
}

impl ComponentInfo {
    pub(crate) fn exponent_mantissa(
        &self,
        sub_band_type: SubBandType,
        resolution: u8,
    ) -> Result<(u16, u16)> {
        let n_ll = self.coding_style.parameters.num_decomposition_levels;

        let sb_index = match sub_band_type {
            SubBandType::LowLow => u16::MAX,
            SubBandType::HighLow => 0,
            SubBandType::LowHigh => 1,
            SubBandType::HighHigh => 2,
        };

        let step_sizes = &self.quantization_info.step_sizes;
        match self.quantization_info.quantization_style {
            QuantizationStyle::NoQuantization | QuantizationStyle::ScalarExpounded => {
                let entry = if resolution == 0 {
                    step_sizes.first()
                } else {
                    step_sizes.get(1 + (resolution as usize - 1) * 3 + sb_index as usize)
                };

                entry
                    .map(|s| (s.exponent, s.mantissa))
                    .ok_or(DecodeError::Malformed)
            }
            QuantizationStyle::ScalarDerived => {
                let (e_0, mantissa) = step_sizes
                    .first()
                    .map(|s| (s.exponent, s.mantissa))
                    .ok_or(DecodeError::Malformed)?;
                let n_b = if resolution == 0 {
                    n_ll as u16
                } else {
                    n_ll as u16 + 1 - resolution as u16
                };

                let exponent = e_0
                    .checked_sub(n_ll as u16)
                    .and_then(|e| e.checked_add(n_b))
                    .ok_or(DecodeError::Malformed)?;

                Ok((exponent, mantissa))
            }
        }
    }

    pub(crate) fn wavelet_transform(&self) -> WaveletTransform {
        self.coding_style.parameters.transformation
    }

    pub(crate) fn num_resolution_levels(&self) -> u8 {
        self.coding_style.parameters.num_resolution_levels
    }

    pub(crate) fn num_decomposition_levels(&self) -> u8 {
        self.coding_style.parameters.num_decomposition_levels
    }

    pub(crate) fn code_block_style(&self) -> CodeBlockStyle {
        self.coding_style.parameters.code_block_style
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ProgressionOrder {
    LayerResolutionComponentPosition,
    ResolutionLayerComponentPosition,
    ResolutionPositionComponentLayer,
    PositionComponentResolutionLayer,
    ComponentPositionResolutionLayer,
}

impl ProgressionOrder {
    fn from_u8(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::LayerResolutionComponentPosition),
            1 => Ok(Self::ResolutionLayerComponentPosition),
            2 => Ok(Self::ResolutionPositionComponentLayer),
            3 => Ok(Self::PositionComponentResolutionLayer),
            4 => Ok(Self::ComponentPositionResolutionLayer),
            _ => Err(DecodeError::Malformed),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProgressionChange {
    pub(crate) resolution_start: u8,
    pub(crate) component_start: u16,
    pub(crate) layer_end: u16,
    pub(crate) resolution_end: u8,
    pub(crate) component_end: u16,
    pub(crate) progression_order: ProgressionOrder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaveletTransform {
    Irreversible97,
    Reversible53,
}

impl WaveletTransform {
    fn from_u8(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Irreversible97),
            1 => Ok(Self::Reversible53),
            _ => Err(DecodeError::Malformed),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CodingStyleFlags {
    pub(crate) raw: u8,
}

impl CodingStyleFlags {
    fn from_u8(value: u8) -> Self {
        Self { raw: value }
    }

    pub(crate) fn has_precincts(&self) -> bool {
        (self.raw & 0x01) != 0
    }

    pub(crate) fn may_use_sop_markers(&self) -> bool {
        (self.raw & 0x02) != 0
    }

    pub(crate) fn uses_eph_marker(&self) -> bool {
        (self.raw & 0x04) != 0
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CodeBlockStyle {
    pub(crate) selective_arithmetic_coding_bypass: bool,
    pub(crate) reset_context_probabilities: bool,
    pub(crate) termination_on_each_pass: bool,
    pub(crate) vertically_causal_context: bool,
    pub(crate) segmentation_symbols: bool,
}

impl CodeBlockStyle {
    fn from_u8(value: u8) -> Self {
        Self {
            selective_arithmetic_coding_bypass: (value & 0x01) != 0,
            reset_context_probabilities: (value & 0x02) != 0,
            termination_on_each_pass: (value & 0x04) != 0,
            vertically_causal_context: (value & 0x08) != 0,
            segmentation_symbols: (value & 0x20) != 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuantizationStyle {
    NoQuantization,
    ScalarDerived,
    ScalarExpounded,
}

impl QuantizationStyle {
    fn from_u8(value: u8) -> Result<Self> {
        match value & 0x1F {
            0 => Ok(Self::NoQuantization),
            1 => Ok(Self::ScalarDerived),
            2 => Ok(Self::ScalarExpounded),
            _ => Err(DecodeError::Malformed),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct StepSize {
    pub(crate) mantissa: u16,
    pub(crate) exponent: u16,
}

#[derive(Clone, Debug)]
pub(crate) struct QuantizationInfo {
    pub(crate) quantization_style: QuantizationStyle,
    pub(crate) guard_bits: u8,
    pub(crate) step_sizes: Vec<StepSize>,
}

#[derive(Debug, Clone)]
pub(crate) struct CodingStyleDefault {
    pub(crate) progression_order: ProgressionOrder,
    pub(crate) num_layers: u8,
    pub(crate) mct: bool,
    pub(crate) component_parameters: CodingStyleComponent,
}

#[derive(Clone, Debug)]
pub(crate) struct CodingStyleComponent {
    pub(crate) flags: CodingStyleFlags,
    pub(crate) parameters: CodingStyleParameters,
}

#[derive(Clone, Debug)]
pub(crate) struct CodingStyleParameters {
    pub(crate) num_decomposition_levels: u8,
    pub(crate) num_resolution_levels: u8,
    pub(crate) code_block_width: u8,
    pub(crate) code_block_height: u8,
    pub(crate) code_block_style: CodeBlockStyle,
    pub(crate) transformation: WaveletTransform,
    pub(crate) precinct_exponents: Vec<(u8, u8)>,
}

#[derive(Debug)]
pub(crate) struct SizeData {
    pub(crate) reference_grid_width: u32,
    pub(crate) reference_grid_height: u32,
    pub(crate) tile_width: u32,
    pub(crate) tile_height: u32,
    pub(crate) num_components: u16,
    pub(crate) resolution_shrink_factor: u32,
}

impl SizeData {
    pub(crate) fn tile_x_coord(&self, idx: u32) -> u32 {
        idx % self.num_x_tiles()
    }

    pub(crate) fn tile_y_coord(&self, idx: u32) -> u32 {
        idx / self.num_x_tiles()
    }

    pub(crate) fn num_x_tiles(&self) -> u32 {
        self.reference_grid_width.div_ceil(self.tile_width)
    }

    pub(crate) fn num_y_tiles(&self) -> u32 {
        self.reference_grid_height.div_ceil(self.tile_height)
    }

    pub(crate) fn num_tiles(&self) -> Result<u32> {
        let tile_count = self
            .num_x_tiles()
            .checked_mul(self.num_y_tiles())
            .ok_or(DecodeError::TooLarge)?;

        Ok(tile_count)
    }

    pub(crate) fn image_width(&self) -> u32 {
        self.reference_grid_width
            .div_ceil(self.resolution_shrink_factor)
    }

    pub(crate) fn image_height(&self) -> u32 {
        self.reference_grid_height
            .div_ceil(self.resolution_shrink_factor)
    }
}

fn size_marker(reader: &mut BitReader<'_>, settings: &DecodeSettings) -> Result<SizeData> {
    let lsiz = reader.read_u16().ok_or(DecodeError::Malformed)?;
    let _ = reader.read_u16().ok_or(DecodeError::Malformed)?;

    let xsiz = reader.read_u32().ok_or(DecodeError::Malformed)?;
    let ysiz = reader.read_u32().ok_or(DecodeError::Malformed)?;
    let x_osiz = reader.read_u32().ok_or(DecodeError::Malformed)?;
    let y_osiz = reader.read_u32().ok_or(DecodeError::Malformed)?;
    let xt_siz = reader.read_u32().ok_or(DecodeError::Malformed)?;
    let yt_siz = reader.read_u32().ok_or(DecodeError::Malformed)?;
    let xto_siz = reader.read_u32().ok_or(DecodeError::Malformed)?;
    let yto_siz = reader.read_u32().ok_or(DecodeError::Malformed)?;
    let csiz = reader.read_u16().ok_or(DecodeError::Malformed)?;

    if u32::from(lsiz) != 38 + 3 * u32::from(csiz) {
        bail!(DecodeError::Malformed);
    }

    if !(1..=4).contains(&csiz) {
        bail!(DecodeError::Unsupported);
    }

    if xsiz == 0 || ysiz == 0 || xt_siz == 0 || yt_siz == 0 {
        bail!(DecodeError::Malformed);
    }

    if xsiz > settings.max_edge || ysiz > settings.max_edge {
        bail!(DecodeError::TooLarge);
    }

    if x_osiz != 0 || y_osiz != 0 || xto_siz != 0 || yto_siz != 0 {
        bail!(DecodeError::Unsupported);
    }

    for _ in 0..csiz {
        let ssiz = reader.read_byte().ok_or(DecodeError::Malformed)?;
        let x_rsiz = reader.read_byte().ok_or(DecodeError::Malformed)?;
        let y_rsiz = reader.read_byte().ok_or(DecodeError::Malformed)?;

        if ssiz != 7 || x_rsiz != 1 || y_rsiz != 1 {
            bail!(DecodeError::Unsupported);
        }
    }

    Ok(SizeData {
        reference_grid_width: xsiz,
        reference_grid_height: ysiz,
        tile_width: xt_siz,
        tile_height: yt_siz,
        num_components: csiz,
        resolution_shrink_factor: 1,
    })
}

fn coding_style_parameters(
    reader: &mut BitReader<'_>,
    coding_style: &CodingStyleFlags,
) -> Result<CodingStyleParameters> {
    let num_decomposition_levels = reader.read_byte().ok_or(DecodeError::Malformed)?;

    if num_decomposition_levels > MAX_DECOMPOSITION_LEVELS {
        bail!(DecodeError::TooLarge);
    }

    let num_resolution_levels = num_decomposition_levels
        .checked_add(1)
        .ok_or(DecodeError::Malformed)?;
    let code_block_width = reader
        .read_byte()
        .and_then(|v| v.checked_add(2))
        .ok_or(DecodeError::Malformed)?;
    let code_block_height = reader
        .read_byte()
        .and_then(|v| v.checked_add(2))
        .ok_or(DecodeError::Malformed)?;

    if code_block_width > 10 || code_block_height > 10 || code_block_width + code_block_height > 12
    {
        bail!(DecodeError::Malformed);
    }

    let code_block_style =
        CodeBlockStyle::from_u8(reader.read_byte().ok_or(DecodeError::Malformed)?);
    let transformation =
        WaveletTransform::from_u8(reader.read_byte().ok_or(DecodeError::Malformed)?)?;

    let mut precinct_exponents = Vec::new();
    if coding_style.has_precincts() {
        for _ in 0..num_resolution_levels {
            let precinct_size = reader.read_byte().ok_or(DecodeError::Malformed)?;
            let width_exp = precinct_size & 0xF;
            let height_exp = precinct_size >> 4;

            if width_exp > MAX_PRECINCT_EXPONENT || height_exp > MAX_PRECINCT_EXPONENT {
                bail!(DecodeError::Malformed);
            }

            precinct_exponents.push((width_exp, height_exp));
        }
    } else {
        for _ in 0..num_resolution_levels {
            precinct_exponents.push((15, 15));
        }
    }

    Ok(CodingStyleParameters {
        num_decomposition_levels,
        num_resolution_levels,
        code_block_width,
        code_block_height,
        code_block_style,
        transformation,
        precinct_exponents,
    })
}

fn com_marker(reader: &mut BitReader<'_>) -> Option<()> {
    skip_marker_segment(reader)
}

fn tlm_marker(reader: &mut BitReader<'_>) -> Option<()> {
    skip_marker_segment(reader)
}

fn ppm_marker<'a>(reader: &mut BitReader<'a>) -> Option<PpmMarkerData<'a>> {
    let segment_len = reader.read_u16()?.checked_sub(2)? as usize;
    let ppm_data = reader.read_bytes(segment_len)?;

    let (&sequence_idx, data) = ppm_data.split_first()?;

    Some(PpmMarkerData { sequence_idx, data })
}

fn ppm_packets(mut markers: Vec<PpmMarkerData<'_>>) -> Option<Vec<PpmPacket>> {
    markers.sort_by_key(|m| m.sequence_idx);
    let data = markers
        .iter()
        .flat_map(|m| m.data)
        .copied()
        .collect::<Vec<_>>();

    let mut reader = BitReader::new(&data);
    let mut packets = vec![];

    while !reader.at_end() {
        let length = reader.read_u32()? as usize;
        packets.push(reader.read_bytes(length)?.to_vec());
    }

    Some(packets)
}

pub(crate) fn rgn_marker(reader: &mut BitReader<'_>) -> Option<(u16, u8)> {
    let _ = reader.read_u16()?;
    let component_index = u16::from(reader.read_byte()?);
    let roi_style = reader.read_byte()?;
    let roi_shift = reader.read_byte()?;

    Some((component_index, if roi_style == 0 { roi_shift } else { 0 }))
}

pub(crate) fn skip_marker_segment(reader: &mut BitReader<'_>) -> Option<()> {
    let length = reader.read_u16()?.checked_sub(2)?;
    reader.skip_bytes(length as usize)?;

    Some(())
}

pub(crate) fn cod_marker(reader: &mut BitReader<'_>) -> Result<CodingStyleDefault> {
    let _ = reader.read_u16().ok_or(DecodeError::Malformed)?;

    let coding_style_flags =
        CodingStyleFlags::from_u8(reader.read_byte().ok_or(DecodeError::Malformed)?);
    let progression_order =
        ProgressionOrder::from_u8(reader.read_byte().ok_or(DecodeError::Malformed)?)?;

    let num_layers = reader.read_u16().ok_or(DecodeError::Malformed)?;

    if num_layers == 0 {
        bail!(DecodeError::Malformed);
    }

    if num_layers > u16::from(MAX_LAYER_COUNT) {
        bail!(DecodeError::TooLarge);
    }

    let mct = reader.read_byte().ok_or(DecodeError::Malformed)? == 1;

    let coding_style_parameters = coding_style_parameters(reader, &coding_style_flags)?;

    Ok(CodingStyleDefault {
        progression_order,
        num_layers: num_layers as u8,
        mct,
        component_parameters: CodingStyleComponent {
            flags: coding_style_flags,
            parameters: coding_style_parameters,
        },
    })
}

pub(crate) fn coc_marker(reader: &mut BitReader<'_>) -> Result<(u16, CodingStyleComponent)> {
    let _ = reader.read_u16().ok_or(DecodeError::Malformed)?;

    let component_index = u16::from(reader.read_byte().ok_or(DecodeError::Malformed)?);
    let coding_style = CodingStyleFlags::from_u8(reader.read_byte().ok_or(DecodeError::Malformed)?);

    let parameters = coding_style_parameters(reader, &coding_style)?;

    let coc = CodingStyleComponent {
        flags: coding_style,
        parameters,
    };

    Ok((component_index, coc))
}

pub(crate) fn poc_marker(reader: &mut BitReader<'_>) -> Option<Vec<ProgressionChange>> {
    let length = reader.read_u16()?.checked_sub(2)?;
    let mut reader = BitReader::new(reader.read_bytes(length as usize)?);

    (0..length as usize / 7)
        .map(|_| {
            Some(ProgressionChange {
                resolution_start: reader.read_byte()?,
                component_start: u16::from(reader.read_byte()?),
                layer_end: reader.read_u16()?,
                resolution_end: reader.read_byte()?,
                component_end: match u16::from(reader.read_byte()?) {
                    0 => 256,
                    index => index,
                },
                progression_order: ProgressionOrder::from_u8(reader.read_byte()?).ok()?,
            })
        })
        .collect()
}

pub(crate) fn qcd_marker(reader: &mut BitReader<'_>) -> Option<QuantizationInfo> {
    let length = reader.read_u16()?;

    let sqcd_val = reader.read_byte()?;
    let quantization_style = QuantizationStyle::from_u8(sqcd_val & 0x1F).ok()?;
    let guard_bits = (sqcd_val >> 5) & 0x07;

    let remaining_bytes = length.checked_sub(3)? as usize;

    let mut parameters = quantization_parameters(reader, quantization_style, remaining_bytes)?;
    parameters.guard_bits = guard_bits;

    Some(parameters)
}

pub(crate) fn qcc_marker(reader: &mut BitReader<'_>) -> Option<(u16, QuantizationInfo)> {
    let length = reader.read_u16()?;
    let component_index = u16::from(reader.read_byte()?);

    let sqcc_val = reader.read_byte()?;
    let quantization_style = QuantizationStyle::from_u8(sqcc_val & 0x1F).ok()?;
    let guard_bits = (sqcc_val >> 5) & 0x07;

    let remaining_bytes = length.checked_sub(4)? as usize;

    let mut parameters = quantization_parameters(reader, quantization_style, remaining_bytes)?;
    parameters.guard_bits = guard_bits;

    Some((component_index, parameters))
}

fn quantization_parameters(
    reader: &mut BitReader<'_>,
    quantization_style: QuantizationStyle,
    remaining_bytes: usize,
) -> Option<QuantizationInfo> {
    let mut step_sizes = Vec::new();

    let irreversible = |val: u16| {
        let exponent = val >> 11;
        let mantissa = val & ((1 << 11) - 1);

        StepSize { exponent, mantissa }
    };

    match quantization_style {
        QuantizationStyle::NoQuantization => {
            for _ in 0..remaining_bytes {
                let value = reader.read_byte()? as u16;
                step_sizes.push(StepSize {
                    mantissa: 0,
                    exponent: (value >> 3),
                });
            }
        }
        QuantizationStyle::ScalarDerived => {
            let value = reader.read_u16()?;
            step_sizes.push(irreversible(value));
        }
        QuantizationStyle::ScalarExpounded => {
            let num_bands = remaining_bytes / 2;
            for _ in 0..num_bands {
                let value = reader.read_u16()?;

                step_sizes.push(irreversible(value));
            }
        }
    }

    Some(QuantizationInfo {
        quantization_style,
        guard_bits: 0,
        step_sizes,
    })
}

#[allow(
    unused,
    reason = "Not all marker codes are used in every decoding path yet"
)]
pub(crate) mod markers {
    pub(crate) const SOC: u8 = 0x4F;
    pub(crate) const SOT: u8 = 0x90;
    pub(crate) const SOD: u8 = 0x93;
    pub(crate) const EOC: u8 = 0xD9;

    pub(crate) const SIZ: u8 = 0x51;

    pub(crate) const COD: u8 = 0x52;
    pub(crate) const COC: u8 = 0x53;
    pub(crate) const RGN: u8 = 0x5E;
    pub(crate) const QCD: u8 = 0x5C;
    pub(crate) const QCC: u8 = 0x5D;
    pub(crate) const POC: u8 = 0x5F;

    pub(crate) const TLM: u8 = 0x55;
    pub(crate) const PLM: u8 = 0x57;
    pub(crate) const PLT: u8 = 0x58;
    pub(crate) const PPM: u8 = 0x60;
    pub(crate) const PPT: u8 = 0x61;

    pub(crate) const SOP: u8 = 0x91;
    pub(crate) const EPH: u8 = 0x92;

    pub(crate) const CRG: u8 = 0x63;
    pub(crate) const COM: u8 = 0x64;

    pub(crate) fn to_string(marker: u8) -> &'static str {
        match marker {
            SOC => "SOC",
            SOT => "SOT",
            SOD => "SOD",
            EOC => "EOC",

            SIZ => "SIZ",

            COD => "COD",
            COC => "COC",
            RGN => "RGN",
            QCD => "QCD",
            QCC => "QCC",
            POC => "POC",

            TLM => "TLM",
            PLM => "PLM",
            PLT => "PLT",
            PPM => "PPM",
            PPT => "PPT",

            SOP => "SOP",
            EPH => "EPH",

            CRG => "CRG",
            COM => "COM",

            _ => "UNKNOWN",
        }
    }
}
