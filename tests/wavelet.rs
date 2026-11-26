use std::fs;
use std::path::PathBuf;

use crate::*;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("benches")
        .join("fixtures")
}

#[test]
fn reduced_resolution_1() {
    check_with(
        "codestream_gray",
        DecodeSettings {
            discard: 1,
            ..Default::default()
        },
        reference("wavelet_reduced_resolution_1"),
    );
}

#[test]
fn reduced_resolution_2() {
    check_with(
        "codestream_gray",
        DecodeSettings {
            discard: 2,
            ..Default::default()
        },
        reference("wavelet_reduced_resolution_2"),
    );
}

#[test]
fn reduced_resolution_3() {
    check_with(
        "codestream_gray",
        DecodeSettings {
            discard: 3,
            ..Default::default()
        },
        reference("wavelet_reduced_resolution_3"),
    );
}

#[test]
fn reduced_resolution_4() {
    check_with(
        "codestream_gray",
        DecodeSettings {
            discard: 4,
            ..Default::default()
        },
        reference("wavelet_reduced_resolution_4"),
    );
}

#[test]
fn reduced_lowest() {
    check_with(
        "codestream_gray",
        DecodeSettings {
            discard: 5,
            ..Default::default()
        },
        reference("wavelet_reduced_lowest"),
    );
}

#[test]
fn reduced_clamped_above_levels() {
    check_with(
        "codestream_gray",
        DecodeSettings {
            discard: 20,
            ..Default::default()
        },
        reference("wavelet_reduced_lowest"),
    );
}

#[test]
fn reduced_per_component_levels() {
    check_with(
        "codestream_levels_per_component",
        DecodeSettings {
            discard: 1,
            ..Default::default()
        },
        reference("wavelet_reduced_per_component_levels"),
    );
}

#[test]
fn fixtures_discard_edges() {
    let mut count = 0_usize;
    for entry in fs::read_dir(fixtures_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("j2c") {
            continue;
        }
        count += 1;
        let data = fs::read(&path).unwrap();
        let full = Image::from_codestream(&data, &DecodeSettings::default()).unwrap();
        let full_w = full.width();
        let full_h = full.height();
        let levels = full.levels();
        assert!(
            full.layers() >= 1,
            "{}: layers must be >= 1",
            path.display()
        );

        for discard in 0..=levels + 1 {
            let settings = DecodeSettings {
                strict: true,
                discard,
                ..DecodeSettings::default()
            };
            let image = Image::from_codestream(&data, &settings).unwrap();
            let clamped = discard.min(levels);
            let expected_w = full_w.div_ceil(1 << clamped);
            let expected_h = full_h.div_ceil(1 << clamped);
            assert_eq!(
                (image.width(), image.height()),
                (expected_w, expected_h),
                "{} discard {discard}",
                path.display()
            );
            let mut ctx = DecoderContext::default();
            let decoded = image.decode(&mut ctx).unwrap();
            let samples = decoded.data_u8().unwrap();
            assert_eq!(
                samples.len(),
                expected_w as usize * expected_h as usize * decoded.components().len(),
                "{} discard {discard} sample count",
                path.display()
            );
        }
    }
    assert!(count > 0, "no bench fixtures found");
}

#[test]
fn max_layers_layered_and_single() {
    let layered_data = fs::read(fixtures_dir().join("rgba-1024-lrcp.j2c")).unwrap();
    let full_img = Image::from_codestream(&layered_data, &DecodeSettings::default()).unwrap();
    assert!(full_img.layers() > 1, "expected multi-layer fixture");

    let all_layers = full_img
        .decode(&mut DecoderContext::default())
        .unwrap()
        .data_u8()
        .unwrap();

    let one_layer_settings = DecodeSettings {
        max_layers: Some(1),
        ..DecodeSettings::default()
    };
    let one_layer_img = Image::from_codestream(&layered_data, &one_layer_settings).unwrap();
    let one_layer = one_layer_img
        .decode(&mut DecoderContext::default())
        .unwrap()
        .data_u8()
        .unwrap();
    assert_ne!(
        one_layer, all_layers,
        "max_layers=1 should differ from all layers on layered fixture"
    );

    let zero_layer_settings = DecodeSettings {
        max_layers: Some(0),
        ..DecodeSettings::default()
    };
    let zero_layer_img = Image::from_codestream(&layered_data, &zero_layer_settings).unwrap();
    let zero_layer = zero_layer_img
        .decode(&mut DecoderContext::default())
        .unwrap()
        .data_u8()
        .unwrap();
    assert_eq!(zero_layer, one_layer, "max_layers=0 should behave like 1");

    let single_data = read_stream("codestream_gray");
    let single_img = Image::new(&single_data, &DecodeSettings::default()).unwrap();
    assert_eq!(single_img.layers(), 1, "expected 1-layer stream");
    let single_all = single_img
        .decode(&mut DecoderContext::default())
        .unwrap()
        .data_u8()
        .unwrap();
    let single_one_img = Image::new(&single_data, &one_layer_settings).unwrap();
    let single_one = single_one_img
        .decode(&mut DecoderContext::default())
        .unwrap()
        .data_u8()
        .unwrap();
    assert_eq!(
        single_one, single_all,
        "max_layers=1 should equal all layers on a 1-layer stream"
    );
}
