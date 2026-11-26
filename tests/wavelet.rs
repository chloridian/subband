use crate::*;

#[test]
fn reduced_resolution_1() {
    check_with(
        "codestream_gray",
        DecodeSettings {
            target_resolution: Some((64, 48)),
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
            target_resolution: Some((32, 24)),
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
            target_resolution: Some((16, 12)),
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
            target_resolution: Some((8, 6)),
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
            target_resolution: Some((4, 3)),
            ..Default::default()
        },
        reference("wavelet_reduced_lowest"),
    );
}

#[test]
fn reduced_target_larger_than_image() {
    check_with(
        "codestream_gray",
        DecodeSettings {
            target_resolution: Some((1000, 1000)),
            ..Default::default()
        },
        reference("main_r"),
    );
}

#[test]
fn reduced_per_component_levels() {
    check_with(
        "codestream_levels_per_component",
        DecodeSettings {
            target_resolution: Some((64, 48)),
            ..Default::default()
        },
        reference("wavelet_reduced_per_component_levels"),
    );
}
