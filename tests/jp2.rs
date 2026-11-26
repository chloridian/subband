use crate::*;

#[test]
fn colr_srgb() {
    check("jp2_colr_srgb", reference("main_rgb"));
}

#[test]
fn colr_greyscale() {
    check("jp2_colr_greyscale", reference("main_r"));
}

#[test]
fn colr_esrgb() {
    check("jp2_colr_esrgb", reference("main_rgb"));
}

#[test]
fn gray_alpha() {
    check("jp2_gray_alpha", reference("main_ra"));
}

#[test]
fn alpha_whole_image() {
    check("jp2_alpha_whole_image", reference("main_ra"));
}

#[test]
fn rgba() {
    check("jp2_rgba", reference("main_rgba"));
}

#[test]
fn jpx_brand_rreq() {
    check("jp2_jpx_brand_rreq", reference("main_rgb"));
}

#[test]
fn extra_boxes() {
    check("jp2_extra_boxes", reference("main_rgb"));
}

#[test]
fn xl_box() {
    check("jp2_xl_box", reference("main_rgb"));
}

#[test]
fn jp2c_length_zero() {
    check("jp2_jp2c_length_zero", reference("main_rgb"));
}

#[test]
fn box_longer_than_file() {
    check("jp2_box_longer_than_file", reference("main_rgb"));
}

#[test]
fn trailing_box_longer_than_file() {
    check("jp2_trailing_box_longer_than_file", reference("main_rgb"));
}

#[test]
fn trailing_zero_box() {
    check("jp2_trailing_zero_box", reference("main_rgb"));
}

#[test]
fn duplicate_ihdr() {
    check("jp2_duplicate_ihdr", reference("main_rgb"));
}
