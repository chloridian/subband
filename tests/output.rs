use std::assert_matches;
use std::fs;
use std::path::PathBuf;

use subband::{DecodeError, DecodeSettings, DecoderContext, Image};

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
fn write_to_matches_data_u8() {
    for &name in FIXTURES {
        let data = fixture(name);
        for discard in [0, 2] {
            let settings = DecodeSettings {
                discard,
                ..DecodeSettings::default()
            };
            let image = Image::from_codestream(&data, &settings)
                .unwrap_or_else(|e| panic!("{name} discard {discard}: parse failed: {e}"));
            let mut ctx = DecoderContext::default();
            let decoded = image
                .decode(&mut ctx)
                .unwrap_or_else(|e| panic!("{name} discard {discard}: decode failed: {e}"));

            let expected_len =
                (image.width() as usize) * (image.height() as usize) * decoded.components().len();
            assert_eq!(
                decoded.buffer_len(),
                expected_len,
                "{name} discard {discard}: buffer_len mismatch"
            );

            let expected = decoded
                .data_u8()
                .unwrap_or_else(|e| panic!("{name} discard {discard}: data_u8 failed: {e}"));
            let mut buf = vec![0; decoded.buffer_len()];
            decoded
                .write_to(&mut buf)
                .unwrap_or_else(|e| panic!("{name} discard {discard}: write_to failed: {e}"));
            assert_eq!(buf, expected, "{name} discard {discard}: output mismatch");
        }
    }
}

#[test]
fn write_to_wrong_size() {
    let data = fixture("gray-alpha");
    let image = Image::from_codestream(&data, &DecodeSettings::default()).expect("fixture parses");
    let mut ctx = DecoderContext::default();
    let decoded = image.decode(&mut ctx).expect("fixture decodes");
    let len = decoded.buffer_len();
    assert!(len > 0);

    for bad_len in [len - 1, len + 1] {
        let mut buf = vec![0xA5; bad_len];
        assert_matches!(
            decoded.write_to(&mut buf),
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
fn write_to_component_counts() {
    let settings = DecodeSettings {
        discard: 2,
        ..DecodeSettings::default()
    };
    for (name, expected_channels) in [
        ("alpha-gradient", 1_usize),
        ("gray-alpha", 2),
        ("white", 3),
        ("rgba-1024-lrcp", 4),
    ] {
        let data = fixture(name);
        let image = Image::from_codestream(&data, &settings).expect("fixture parses");
        let mut ctx = DecoderContext::default();
        let decoded = image.decode(&mut ctx).expect("fixture decodes");
        assert_eq!(
            decoded.components().len(),
            expected_channels,
            "{name}: unexpected component count"
        );
        let expected_len = (image.width() as usize) * (image.height() as usize) * expected_channels;
        assert_eq!(
            decoded.buffer_len(),
            expected_len,
            "{name}: unexpected buffer_len"
        );
        let mut buf = vec![0; expected_len];
        decoded
            .write_to(&mut buf)
            .unwrap_or_else(|e| panic!("{name}: write_to failed: {e}"));
        let expected = decoded.data_u8().expect("data_u8 succeeds");
        assert_eq!(buf, expected, "{name}: output mismatch");
    }
}
