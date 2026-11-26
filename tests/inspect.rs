use std::assert_matches;
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use subband::{DecodeError, DecodeSettings, DecoderContext, Image};

use crate::*;

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("benches")
        .join("fixtures")
        .join(format!("{name}.j2c"));
    fs::read(&path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

fn decode_bytes(data: &[u8], discard: u8, max_layers: Option<u8>) -> Vec<u8> {
    let settings = DecodeSettings {
        discard,
        max_layers,
        ..DecodeSettings::default()
    };
    let image = Image::from_codestream(data, &settings).unwrap();
    let mut ctx = DecoderContext::default();
    image.decode(&mut ctx).unwrap().data_u8().unwrap()
}

fn verify_fixture_prefixes(name: &str, expect_plt_ahead: bool) {
    let data = fixture(name);
    let settings = DecodeSettings::default();
    let full = subband::inspect(&data, &settings).unwrap();
    let levels = full.levels();
    let layers = full.layers();

    assert_eq!(
        full.complete_discard(),
        Some(0),
        "{name}: full file should complete discard 0"
    );

    let mut boundaries = BTreeSet::new();
    for d in 0..=levels {
        let all_end = full
            .bytes_for(d, None)
            .unwrap_or_else(|| panic!("{name}: missing bytes_for({d}, None)"));
        assert!(
            all_end <= data.len(),
            "{name}: bytes_for({d}, None) exceeds file"
        );
        boundaries.insert(all_end);
        for l in 1..=layers {
            let layer_end = full
                .bytes_for(d, Some(l))
                .unwrap_or_else(|| panic!("{name}: missing bytes_for({d}, Some({l}))"));
            assert!(
                layer_end <= all_end,
                "{name}: layer {l} end > all layers end"
            );
            boundaries.insert(layer_end);
        }
    }

    assert!(!boundaries.is_empty(), "{name}: expected packet boundaries");

    if expect_plt_ahead {
        let first_boundary = *boundaries.first().unwrap();
        let early = subband::inspect(&data[..first_boundary], &settings).unwrap();
        for d in 0..=levels {
            assert_eq!(
                early.bytes_for(d, None),
                full.bytes_for(d, None),
                "{name}: PLT should determine bytes_for({d}, None) from early prefix"
            );
        }
    }

    for &b in &boundaries {
        for len in [b.saturating_sub(1), b] {
            let info = subband::inspect(&data[..len], &settings).unwrap();
            assert_eq!(info.width(), full.width());
            assert_eq!(info.height(), full.height());
            assert_eq!(info.components(), full.components());
            assert_eq!(info.levels(), levels);
            assert_eq!(info.layers(), layers);

            for d in 0..=levels {
                let req = full.bytes_for(d, None).unwrap();
                if len >= req {
                    assert!(
                        info.complete_discard().is_some_and(|c| c <= d),
                        "{name}: prefix {len} >= bytes_for({d})={req}, got complete_discard={:?}",
                        info.complete_discard()
                    );
                } else {
                    assert!(
                        info.complete_discard().is_none_or(|c| c > d),
                        "{name}: prefix {len} < bytes_for({d})={req}, got complete_discard={:?}",
                        info.complete_discard()
                    );
                }
                if let Some(known) = info.bytes_for(d, None) {
                    assert_eq!(known, req, "{name}: prefix {len} bytes_for({d}) mismatch");
                }
            }
        }
    }

    for d in 0..=levels {
        let req = full.bytes_for(d, None).unwrap();
        let expected = decode_bytes(&data, d, None);
        let actual = decode_bytes(&data[..req], d, None);
        assert_eq!(
            actual, expected,
            "{name}: prefix decode at discard {d} (len {req}) differs from full file"
        );

        let layer1_req = full.bytes_for(d, Some(1)).unwrap();
        let expected_l1 = decode_bytes(&data, d, Some(1));
        let actual_l1 = decode_bytes(&data[..layer1_req], d, Some(1));
        assert_eq!(
            actual_l1, expected_l1,
            "{name}: prefix decode at discard {d}, layers=1 (len {layer1_req}) differs from full file"
        );
    }
}

#[test]
fn prefixes_rgb_rlcp() {
    verify_fixture_prefixes("rgb-1024-rlcp", false);
}

#[test]
fn prefixes_rgba_lrcp_plt() {
    verify_fixture_prefixes("rgba-1024-lrcp", true);
}

#[test]
fn prefixes_plywood() {
    verify_fixture_prefixes("plywood", false);
}

#[test]
fn multi_tile_and_progressions() {
    for name in [
        "codestream_tiles_partial",
        "codestream_plt",
        "packets_progression_lrcp",
        "packets_progression_rlcp",
        "packets_progression_rpcl",
        "packets_progression_pcrl",
        "packets_progression_cprl",
        "tile_parts_interleaved",
    ] {
        let data = read_stream(name);
        let full = subband::inspect(&data, &DecodeSettings::default()).unwrap();
        assert_eq!(full.complete_discard(), Some(0), "{name}: complete_discard");
        for d in 0..=full.levels() {
            let req = full
                .bytes_for(d, None)
                .unwrap_or_else(|| panic!("{name}: bytes_for({d}, None)"));
            let expected = decode_bytes(&data, d, None);
            let actual = decode_bytes(&data[..req], d, None);
            assert_eq!(actual, expected, "{name}: discard {d} prefix mismatch");
        }
    }
}

#[test]
fn truncated_headers_and_limits() {
    let data = fixture("plywood");
    let settings = DecodeSettings::default();

    for cut in [0, 1, 2, 4, 20, 50] {
        let res = subband::inspect(&data[..cut], &settings);
        assert_matches!(res, Err(DecodeError::Malformed));
    }

    let sot_pos = data
        .windows(2)
        .position(|w| w == [0xFF, 0x90])
        .expect("plywood has SOT");
    assert_matches!(
        subband::inspect(&data[..sot_pos], &settings),
        Err(DecodeError::Malformed)
    );

    let after_sot = subband::inspect(&data[..sot_pos + 2], &settings).unwrap();
    assert_eq!(after_sot.complete_discard(), None);
    assert_eq!(after_sot.bytes_for(after_sot.levels(), None), None);

    let after_sot_header = subband::inspect(&data[..sot_pos + 12], &settings).unwrap();
    assert_eq!(after_sot_header.complete_discard(), None);
    assert_eq!(
        after_sot_header.bytes_for(after_sot_header.levels(), None),
        None
    );

    let small_limit = DecodeSettings {
        max_input: data.len() - 1,
        ..DecodeSettings::default()
    };
    assert_matches!(
        subband::inspect(&data, &small_limit),
        Err(DecodeError::TooLarge)
    );
}
