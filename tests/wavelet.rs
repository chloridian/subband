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
        DecodeSettings::default(),
        Decode {
            discard: 1,
            ..Decode::default()
        },
        reference("wavelet_reduced_resolution_1"),
    );
}

#[test]
fn reduced_resolution_2() {
    check_with(
        "codestream_gray",
        DecodeSettings::default(),
        Decode {
            discard: 2,
            ..Decode::default()
        },
        reference("wavelet_reduced_resolution_2"),
    );
}

#[test]
fn reduced_resolution_3() {
    check_with(
        "codestream_gray",
        DecodeSettings::default(),
        Decode {
            discard: 3,
            ..Decode::default()
        },
        reference("wavelet_reduced_resolution_3"),
    );
}

#[test]
fn reduced_resolution_4() {
    check_with(
        "codestream_gray",
        DecodeSettings::default(),
        Decode {
            discard: 4,
            ..Decode::default()
        },
        reference("wavelet_reduced_resolution_4"),
    );
}

#[test]
fn reduced_lowest() {
    check_with(
        "codestream_gray",
        DecodeSettings::default(),
        Decode {
            discard: 5,
            ..Decode::default()
        },
        reference("wavelet_reduced_lowest"),
    );
}

#[test]
fn reduced_clamped_above_levels() {
    check_with(
        "codestream_gray",
        DecodeSettings::default(),
        Decode {
            discard: 20,
            ..Decode::default()
        },
        reference("wavelet_reduced_lowest"),
    );
}

#[test]
fn reduced_per_component_levels() {
    check_with(
        "codestream_levels_per_component",
        DecodeSettings::default(),
        Decode {
            discard: 1,
            ..Decode::default()
        },
        reference("wavelet_reduced_per_component_levels"),
    );
}

#[test]
fn fixtures_discard_edges() {
    let mut count = 0_usize;
    let settings = DecodeSettings {
        strict: true,
        ..DecodeSettings::default()
    };
    for entry in fs::read_dir(fixtures_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("j2c") {
            continue;
        }
        count += 1;
        let data = fs::read(&path).unwrap();
        let image = Image::from_codestream(&data, &settings).unwrap();
        let full_w = image.width();
        let full_h = image.height();
        let levels = image.levels();
        assert!(
            image.layers() >= 1,
            "{}: layers must be >= 1",
            path.display()
        );

        for discard in 0..=levels + 1 {
            let request = Decode {
                discard,
                max_layers: None,
            };
            let clamped = discard.min(levels);
            let expected_w = full_w.div_ceil(1 << clamped);
            let expected_h = full_h.div_ceil(1 << clamped);
            assert_eq!(
                image.output_size(&request),
                [expected_w, expected_h],
                "{} discard {discard}",
                path.display()
            );
            let expected_len =
                expected_w as usize * expected_h as usize * usize::from(image.components());
            assert_eq!(
                image.output_len(&request),
                expected_len,
                "{} discard {discard} output_len",
                path.display()
            );
            let mut samples = vec![0; expected_len];
            let decoded = image.decode_into(&request, &mut samples, || false).unwrap();
            assert_eq!(
                (decoded.width(), decoded.height()),
                (expected_w, expected_h),
                "{} discard {discard} decoded size",
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

    let all_req = Decode::default();
    let mut all_layers = vec![0; full_img.output_len(&all_req)];
    full_img
        .decode_into(&all_req, &mut all_layers, || false)
        .unwrap();

    let one_req = Decode {
        max_layers: Some(1),
        ..Decode::default()
    };
    let mut one_layer = vec![0; full_img.output_len(&one_req)];
    full_img
        .decode_into(&one_req, &mut one_layer, || false)
        .unwrap();
    assert_ne!(
        one_layer, all_layers,
        "max_layers=1 should differ from all layers on layered fixture"
    );

    let zero_req = Decode {
        max_layers: Some(0),
        ..Decode::default()
    };
    let mut zero_layer = vec![0; full_img.output_len(&zero_req)];
    full_img
        .decode_into(&zero_req, &mut zero_layer, || false)
        .unwrap();
    assert_eq!(zero_layer, one_layer, "max_layers=0 should behave like 1");

    let single_data = read_stream("codestream_gray");
    let single_img = Image::new(&single_data, &DecodeSettings::default()).unwrap();
    assert_eq!(single_img.layers(), 1, "expected 1-layer stream");
    let mut single_all = vec![0; single_img.output_len(&all_req)];
    single_img
        .decode_into(&all_req, &mut single_all, || false)
        .unwrap();
    let mut single_one = vec![0; single_img.output_len(&one_req)];
    single_img
        .decode_into(&one_req, &mut single_one, || false)
        .unwrap();
    assert_eq!(
        single_one, single_all,
        "max_layers=1 should equal all layers on a 1-layer stream"
    );
}
