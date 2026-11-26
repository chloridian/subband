use subband::{DecodeError, Image};

use crate::*;

fn siz_codestream(
    width: u32,
    height: u32,
    x_offset: u32,
    y_offset: u32,
    tile_width: u32,
    tile_height: u32,
    tile_x_offset: u32,
    tile_y_offset: u32,
    components: &[(u8, u8, u8)],
) -> Vec<u8> {
    let csiz = components.len() as u16;
    let lsiz = 38 + 3 * csiz;
    let mut out = Vec::new();
    out.extend_from_slice(&[0xFF, 0x4F, 0xFF, 0x51]);
    out.extend_from_slice(&lsiz.to_be_bytes());
    out.extend_from_slice(&0_u16.to_be_bytes());
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&x_offset.to_be_bytes());
    out.extend_from_slice(&y_offset.to_be_bytes());
    out.extend_from_slice(&tile_width.to_be_bytes());
    out.extend_from_slice(&tile_height.to_be_bytes());
    out.extend_from_slice(&tile_x_offset.to_be_bytes());
    out.extend_from_slice(&tile_y_offset.to_be_bytes());
    out.extend_from_slice(&csiz.to_be_bytes());
    for &(ssiz, xrsiz, yrsiz) in components {
        out.push(ssiz);
        out.push(xrsiz);
        out.push(yrsiz);
    }
    out
}

#[test]
fn too_few_exponents() {
    check_error(
        "errors_too_few_exponents",
        DecodeSettings::default(),
        "malformed codestream",
    );
}

#[test]
fn reduced_below_tile_levels() {
    check_error(
        "codestream_tile_cod_fewer_levels",
        DecodeSettings {
            target_resolution: Some((32, 24)),
            ..Default::default()
        },
        "malformed codestream",
    );
}

#[test]
fn jp2_invalid_codestream() {
    check_error(
        "errors_jp2_invalid_codestream",
        DecodeSettings::default(),
        "malformed codestream",
    );
}

#[test]
fn unsupported_bit_depth() {
    for ssiz in [3_u8, 11, 15] {
        let data = siz_codestream(16, 16, 0, 0, 16, 16, 0, 0, &[(ssiz, 1, 1)]);
        let err = Image::new(&data, &DecodeSettings::default())
            .err()
            .expect("non-8-bit sample precision should fail");
        assert_eq!(err, DecodeError::Unsupported);
    }
}

#[test]
fn unsupported_signed_samples() {
    let data = siz_codestream(16, 16, 0, 0, 16, 16, 0, 0, &[(0x87, 1, 1)]);
    let err = Image::new(&data, &DecodeSettings::default())
        .err()
        .expect("signed samples should fail");
    assert_eq!(err, DecodeError::Unsupported);
}

#[test]
fn unsupported_subsampling() {
    for (xrsiz, yrsiz) in [(2, 1), (1, 2), (2, 2)] {
        let data = siz_codestream(16, 16, 0, 0, 16, 16, 0, 0, &[(7, 1, 1), (7, xrsiz, yrsiz)]);
        let err = Image::new(&data, &DecodeSettings::default())
            .err()
            .expect("subsampled components should fail");
        assert_eq!(err, DecodeError::Unsupported);
    }
}

#[test]
fn unsupported_image_offset() {
    for (x_off, y_off) in [(1, 0), (0, 1)] {
        let data = siz_codestream(16, 16, x_off, y_off, 16, 16, 0, 0, &[(7, 1, 1)]);
        let err = Image::new(&data, &DecodeSettings::default())
            .err()
            .expect("non-zero image offset should fail");
        assert_eq!(err, DecodeError::Unsupported);
    }
}

#[test]
fn unsupported_tile_offset() {
    for (tx_off, ty_off) in [(1, 0), (0, 1)] {
        let data = siz_codestream(16, 16, 0, 0, 16, 16, tx_off, ty_off, &[(7, 1, 1)]);
        let err = Image::new(&data, &DecodeSettings::default())
            .err()
            .expect("non-zero tile offset should fail");
        assert_eq!(err, DecodeError::Unsupported);
    }
}

#[test]
fn unsupported_component_count() {
    let data = siz_codestream(
        16,
        16,
        0,
        0,
        16,
        16,
        0,
        0,
        &[(7, 1, 1), (7, 1, 1), (7, 1, 1), (7, 1, 1), (7, 1, 1)],
    );
    let err = Image::new(&data, &DecodeSettings::default())
        .err()
        .expect("more than 4 components should fail");
    assert_eq!(err, DecodeError::Unsupported);
}

fn stream(name: &str) -> Vec<u8> {
    std::fs::read(stream_path(name)).unwrap()
}

fn too_large(data: &[u8], settings: &DecodeSettings) {
    let err = Image::new(data, settings)
        .and_then(|image| image.decode(&mut DecoderContext::default()).map(drop))
        .expect_err("limit should reject");
    assert_eq!(err, DecodeError::TooLarge);
}

#[test]
fn edge_over_limit() {
    let data = siz_codestream(4097, 16, 0, 0, 4097, 16, 0, 0, &[(7, 1, 1)]);
    too_large(&data, &DecodeSettings::default());
}

#[test]
fn edge_over_custom_limit() {
    let data = siz_codestream(65, 16, 0, 0, 65, 16, 0, 0, &[(7, 1, 1)]);
    let settings = DecodeSettings {
        max_edge: 64,
        ..DecodeSettings::default()
    };
    too_large(&data, &settings);
}

#[test]
fn input_over_limit() {
    let data = stream("errors_too_few_exponents");
    let settings = DecodeSettings {
        max_input: data.len() - 1,
        ..DecodeSettings::default()
    };
    too_large(&data, &settings);
}

#[test]
fn output_over_limit() {
    let data = stream("codestream_tile_size_huge");
    let settings = DecodeSettings {
        max_output: 128 * 96 - 1,
        ..DecodeSettings::default()
    };
    too_large(&data, &settings);
}

#[test]
fn too_many_tiles() {
    let data = siz_codestream(4096, 4096, 0, 0, 16, 16, 0, 0, &[(7, 1, 1)]);
    too_large(&data, &DecodeSettings::default());
}

#[test]
fn too_many_levels() {
    let mut data = stream("errors_too_few_exponents");
    data[54] = 13;
    too_large(&data, &DecodeSettings::default());
}
