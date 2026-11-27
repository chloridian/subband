use std::assert_matches;
use std::fs;
use std::path::PathBuf;

use subband::{Decode, DecodeError, DecodeSettings, Image};

const FIXTURES: &[&str] = &[
    "alpha-gradient",
    "gray-4096",
    "gray-alpha",
    "plywood",
    "profile-picture",
    "rgb-1024-rlcp",
    "rgba-1024-lrcp",
    "rgba-1024x512-rlcp",
    "rgba-2048-lossy",
    "sculpt-64",
    "terrain-grass",
    "water-normal",
    "white",
];

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("benches")
        .join("fixtures")
        .join(format!("{name}.j2c"));
    fs::read(&path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

#[test]
fn dirty_buffer_matches_zeroed() {
    let settings = DecodeSettings::default();
    for &name in FIXTURES {
        let data = fixture(name);
        let image = Image::from_codestream(&data, &settings)
            .unwrap_or_else(|e| panic!("{name}: parse failed: {e}"));
        for discard in [0, 2] {
            let request = Decode {
                discard,
                max_layers: None,
            };
            let [w, h] = image.output_size(&request);
            let expected_len = (w as usize) * (h as usize) * usize::from(image.components());
            assert_eq!(
                image.output_len(&request),
                expected_len,
                "{name} discard {discard}: output_len mismatch"
            );

            let mut zeroed = vec![0; expected_len];
            let decoded = image
                .decode_into(&request, &mut zeroed, || false)
                .unwrap_or_else(|e| panic!("{name} discard {discard}: decode failed: {e}"));
            assert_eq!(decoded.width(), w);
            assert_eq!(decoded.height(), h);
            assert_eq!(decoded.components(), image.components());

            let mut dirty = vec![0xA5; expected_len];
            image
                .decode_into(&request, &mut dirty, || false)
                .unwrap_or_else(|e| panic!("{name} discard {discard}: dirty decode failed: {e}"));
            assert_eq!(dirty, zeroed, "{name} discard {discard}: output mismatch");
        }
    }
}

#[test]
fn wrong_size() {
    let data = fixture("gray-alpha");
    let image = Image::from_codestream(&data, &DecodeSettings::default()).expect("fixture parses");
    let request = Decode::default();
    let len = image.output_len(&request);
    assert!(len > 0);

    for bad_len in [len - 1, len + 1] {
        let mut buf = vec![0xA5; bad_len];
        assert_matches!(
            image.decode_into(&request, &mut buf, || false),
            Err(DecodeError::BufferSize),
            "buffer len {bad_len} should fail with BufferSize"
        );
        assert!(
            buf.iter().all(|&b| b == 0xA5),
            "buffer len {bad_len} was modified on error"
        );
    }
}

#[test]
fn component_counts() {
    let settings = DecodeSettings::default();
    let request = Decode {
        discard: 2,
        max_layers: None,
    };
    for (name, expected_channels) in [
        ("alpha-gradient", 1_u8),
        ("gray-alpha", 2),
        ("white", 3),
        ("rgba-1024-lrcp", 4),
    ] {
        let data = fixture(name);
        let image = Image::from_codestream(&data, &settings).expect("fixture parses");
        assert_eq!(
            image.components(),
            expected_channels,
            "{name}: unexpected component count"
        );
        let [w, h] = image.output_size(&request);
        let expected_len = (w as usize) * (h as usize) * usize::from(expected_channels);
        assert_eq!(
            image.output_len(&request),
            expected_len,
            "{name}: unexpected output_len"
        );
        let mut buf = vec![0; expected_len];
        let decoded = image
            .decode_into(&request, &mut buf, || false)
            .unwrap_or_else(|e| panic!("{name}: decode_into failed: {e}"));
        assert_eq!(decoded.components(), expected_channels);
        assert_eq!(decoded.width(), w);
        assert_eq!(decoded.height(), h);
    }
}

#[test]
fn prefix_dirty_buffer_matches_zeroed() {
    let settings = DecodeSettings::default();
    for name in ["plywood", "rgba-1024-lrcp", "rgb-1024-rlcp"] {
        let data = fixture(name);
        for discard in [0, 2] {
            let request = Decode {
                discard,
                max_layers: None,
            };
            for cut in [256, 600, 2000, 8192] {
                let Some(prefix) = data.get(..cut) else {
                    continue;
                };
                let Ok(image) = Image::from_codestream(prefix, &settings) else {
                    continue;
                };

                let len = image.output_len(&request);
                let mut zeroed = vec![0; len];
                image
                    .decode_into(&request, &mut zeroed, || false)
                    .unwrap_or_else(|e| panic!("{name} d{discard} cut {cut}: zeroed failed: {e}"));
                let mut dirty = vec![0xA5; len];
                image
                    .decode_into(&request, &mut dirty, || false)
                    .unwrap_or_else(|e| panic!("{name} d{discard} cut {cut}: dirty failed: {e}"));
                assert_eq!(
                    dirty, zeroed,
                    "{name} d{discard} cut {cut}: dirty buffer mismatch"
                );
            }
        }
    }
}
