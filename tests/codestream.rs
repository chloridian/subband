use crate::*;

#[test]
fn gray() {
    check("codestream_gray", reference("main_r"));
}

#[test]
fn odd_size() {
    check("codestream_odd_size", reference("main_r_crop_5_3_61_47"));
}

#[test]
fn tiny() {
    check("codestream_tiny", reference("main_r_crop_60_40_3_5"));
}

#[test]
fn single_row() {
    check("codestream_single_row", reference("main_r_crop_0_45_128_1"));
}

#[test]
fn tiles_partial() {
    check("codestream_tiles_partial", reference("main_rgb"));
}

#[test]
fn tile_larger_than_image() {
    check("codestream_tile_larger_than_image", reference("main_rgb"));
}

#[test]
fn levels_0() {
    check("codestream_levels_0", reference("main_r"));
}

#[test]
fn levels_1() {
    check("codestream_levels_1", reference("main_r"));
}

#[test]
fn levels_8() {
    check("codestream_levels_8", reference("main_r"));
}

#[test]
fn levels_more_than_size() {
    check(
        "codestream_levels_more_than_size",
        reference("main_r_crop_10_10_17_9"),
    );
}

#[test]
fn codeblock_4x16() {
    check("codestream_codeblock_4x16", reference("main_r"));
}

#[test]
fn codeblock_8x64() {
    check("codestream_codeblock_8x64", reference("main_r"));
}

#[test]
fn codeblock_64x32() {
    check("codestream_codeblock_64x32", reference("main_r"));
}

#[test]
fn codeblock_32x32() {
    check("codestream_codeblock_32x32", reference("main_r"));
}

#[test]
fn codeblock_128x32() {
    check("codestream_codeblock_128x32", reference("main_r"));
}

#[test]
fn codeblock_precinct_limited() {
    check("codestream_codeblock_precinct_limited", reference("main_r"));
}

#[test]
fn precincts() {
    check("codestream_precincts", reference("main_rgb"));
}

#[test]
fn precincts_non_square() {
    check("codestream_precincts_non_square", reference("main_r"));
}

#[test]
fn precincts_1x1() {
    check(
        "codestream_precincts_1x1",
        reference("main_r_crop_40_30_8_12"),
    );
}

#[test]
fn guard_bits_zero() {
    check("codestream_guard_bits_zero", reference("main_r"));
}

#[test]
fn guard_bits_high() {
    check("codestream_guard_bits_high", reference("main_r"));
}

#[test]
fn tlm() {
    check("codestream_tlm", reference("main_rgb"));
}

#[test]
fn plt() {
    check("codestream_plt", reference("main_rgb"));
}

#[test]
fn crg() {
    check("codestream_crg", reference("main_rgb"));
}

#[test]
fn reserved_marker() {
    check("codestream_reserved_marker", reference("main_r"));
}

#[test]
fn tile_size_huge() {
    check("codestream_tile_size_huge", reference("main_r"));
}

#[test]
fn tile_cod_progression() {
    check("codestream_tile_cod_progression", reference("main_rgb"));
}

#[test]
fn tile_cod_layers() {
    check("codestream_tile_cod_layers", reference("main_r"));
}

#[test]
fn tile_cod_fewer_levels() {
    check("codestream_tile_cod_fewer_levels", reference("main_r"));
}

#[test]
fn tile_qcd() {
    check("codestream_tile_qcd", reference("main_r"));
}

#[test]
fn levels_per_component() {
    check("codestream_levels_per_component", reference("main_rgb"));
}

#[test]
fn coc_style_per_component() {
    check("codestream_coc_style_per_component", reference("main_rgb"));
}

#[test]
fn qcc_per_component() {
    check("codestream_qcc_per_component", reference("main_rgb"));
}

#[test]
fn tile_coc_qcc() {
    check("codestream_tile_coc_qcc", reference("main_rgb"));
}

#[test]
fn com_in_tile_part() {
    check("codestream_com_in_tile_part", reference("main_r"));
}

#[test]
fn coc_wavelet_per_component() {
    check(
        "codestream_coc_wavelet_per_component",
        reference("main_rgb").tolerance(1),
    );
}
