use std::assert_matches;
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use subband::{DecodeError, DecodeSettings, DecoderContext, Image};

const EVEN_CUTS: usize = 64;

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

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("benches")
        .join("fixtures")
}

fn fixture(name: &str) -> Vec<u8> {
    let path = fixtures_dir().join(format!("{name}.j2c"));
    fs::read(&path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

fn main_header_end(data: &[u8]) -> usize {
    assert!(data.starts_with(&[0xFF, 0x4F]), "missing SOC");
    let mut i = 2;
    while i + 1 < data.len() {
        assert_eq!(data[i], 0xFF, "expected marker at {i}");
        let marker = data[i + 1];
        if marker == 0x90 {
            return i + 2;
        }
        i += 2;
        if (0x30..=0x3F).contains(&marker) {
            continue;
        }
        let len = usize::from(u16::from_be_bytes([data[i], data[i + 1]]));
        i += len;
    }
    panic!("SOT marker not found");
}

fn prefix_lengths(data: &[u8], header_end: usize) -> Vec<usize> {
    if data.len() < 4096 {
        return (header_end..=data.len()).collect();
    }

    let span = data.len() - header_end;
    let mut lengths: BTreeSet<usize> = (0..=EVEN_CUTS)
        .map(|i| header_end + span * i / EVEN_CUTS)
        .collect();

    let full = subband::inspect(data, &DecodeSettings::default()).expect("full inspect");
    for d in 0..=full.levels() {
        if let Some(end) = full.bytes_for(d, None)
            && end >= header_end
        {
            lengths.extend([end - 1, end, end + 1]);
        }
        for l in 1..=full.layers() {
            if let Some(end) = full.bytes_for(d, Some(l))
                && end >= header_end
            {
                lengths.extend([end - 1, end, end + 1]);
            }
        }
    }

    lengths
        .into_iter()
        .filter(|&len| (header_end..=data.len()).contains(&len))
        .collect()
}

fn verify_prefixes(name: &str, discard: u8, strict: bool) {
    let data = fixture(name);
    let header_end = main_header_end(&data);
    let settings = DecodeSettings {
        strict,
        discard,
        ..DecodeSettings::default()
    };

    let full_image = Image::from_codestream(&data, &settings).expect("full header parses");
    let expected_width = full_image.width();
    let expected_height = full_image.height();
    let expected_resolutions =
        usize::from(full_image.levels() - discard.min(full_image.levels())) + 1;
    let full_layers = full_image.layers();
    let expected_samples = expected_width as usize * expected_height as usize;

    let lengths = prefix_lengths(&data, header_end);

    for len in lengths {
        let prefix = &data[..len];
        let image = Image::from_codestream(prefix, &settings).unwrap_or_else(|e| {
            panic!("{name} (len={len}, discard={discard}, strict={strict}): header failed: {e}")
        });
        assert_eq!(image.width(), expected_width, "{name} len={len}: width");
        assert_eq!(image.height(), expected_height, "{name} len={len}: height");

        let mut ctx = DecoderContext::default();
        let decoded = image.decode(&mut ctx).unwrap_or_else(|e| {
            panic!("{name} (len={len}, discard={discard}, strict={strict}): decode failed: {e}")
        });
        assert_eq!(
            decoded.layers_received().len(),
            expected_resolutions,
            "{name} len={len}: layers_received len"
        );
        for &l in decoded.layers_received() {
            assert!(
                l <= full_layers,
                "{name} len={len}: layers_received {l} > {full_layers}"
            );
        }
        for comp in decoded.components() {
            assert_eq!(
                comp.samples().len(),
                expected_samples,
                "{name} len={len}: component sample count"
            );
        }
        let out = decoded.data_u8().expect("not cancelled");
        assert_eq!(
            out.len(),
            expected_samples * decoded.components().len(),
            "{name} len={len}: output byte count"
        );
    }
}

macro_rules! fixture_prefix_tests {
    ($mod_name:ident, $fixture:literal) => {
        mod $mod_name {
            use super::verify_prefixes;

            #[test]
            fn d0_lenient() {
                verify_prefixes($fixture, 0, false);
            }

            #[test]
            fn d0_strict() {
                verify_prefixes($fixture, 0, true);
            }

            #[test]
            fn d2_lenient() {
                verify_prefixes($fixture, 2, false);
            }

            #[test]
            fn d2_strict() {
                verify_prefixes($fixture, 2, true);
            }
        }
    };
}

fixture_prefix_tests!(alpha_gradient, "alpha-gradient");
fixture_prefix_tests!(gray_4096, "gray-4096");
fixture_prefix_tests!(gray_alpha, "gray-alpha");
fixture_prefix_tests!(plywood, "plywood");
fixture_prefix_tests!(profile_picture, "profile-picture");
fixture_prefix_tests!(rgb_1024_rlcp, "rgb-1024-rlcp");
fixture_prefix_tests!(rgba_1024_lrcp, "rgba-1024-lrcp");
fixture_prefix_tests!(rgba_1024x512_rlcp, "rgba-1024x512-rlcp");
fixture_prefix_tests!(rgba_2048_lossy, "rgba-2048-lossy");
fixture_prefix_tests!(sculpt_64, "sculpt-64");
fixture_prefix_tests!(terrain_grass, "terrain-grass");
fixture_prefix_tests!(water_normal, "water-normal");
fixture_prefix_tests!(white, "white");

#[test]
fn all_fixtures_listed() {
    let mut actual: Vec<String> = fs::read_dir(fixtures_dir())
        .expect("read fixtures dir")
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) == Some("j2c") {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .map(str::to_string)
            } else {
                None
            }
        })
        .collect();
    actual.sort();
    let expected: Vec<String> = FIXTURES.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(actual, expected, "update FIXTURES list in tests/prefix.rs");
}

#[test]
fn main_header_cut_malformed() {
    for &name in FIXTURES {
        let data = fixture(name);
        let header_end = main_header_end(&data);
        let cuts = [0, 1, 2, 4, header_end / 2, header_end - 2, header_end - 1];

        for strict in [false, true] {
            let settings = DecodeSettings {
                strict,
                ..DecodeSettings::default()
            };
            for cut in cuts {
                if cut >= header_end {
                    continue;
                }
                let res = Image::from_codestream(&data[..cut], &settings).err();
                assert_matches!(
                    res,
                    Some(DecodeError::Malformed),
                    "{name}: cut {cut} (< {header_end}), strict={strict} should be Malformed"
                );
            }
        }
    }
}

#[test]
fn layers_received_full_and_short() {
    let data = fixture("rgba-1024-lrcp");
    let header_end = main_header_end(&data);

    for discard in [0_u8, 2] {
        let settings = DecodeSettings {
            strict: true,
            discard,
            ..DecodeSettings::default()
        };
        let full_image = Image::from_codestream(&data, &settings).unwrap();
        let expected_len = usize::from(full_image.levels() - discard) + 1;
        let total_layers = full_image.layers();
        assert!(total_layers > 1, "rgba-1024-lrcp should be multi-layer");

        let mut ctx = DecoderContext::default();
        let full_decoded = full_image.decode(&mut ctx).unwrap();
        assert_eq!(
            full_decoded.layers_received(),
            vec![total_layers; expected_len].as_slice(),
            "full file discard={discard} should receive all layers"
        );

        let header_only = Image::from_codestream(&data[..header_end], &settings).unwrap();
        let mut ctx = DecoderContext::default();
        let header_decoded = header_only.decode(&mut ctx).unwrap();
        assert_eq!(
            header_decoded.layers_received(),
            vec![0; expected_len].as_slice(),
            "header-only prefix discard={discard} should receive 0 layers"
        );

        let short_image = Image::from_codestream(&data[..2000], &settings).unwrap();
        let mut ctx = DecoderContext::default();
        let short_decoded = short_image.decode(&mut ctx).unwrap();
        assert_eq!(short_decoded.layers_received().len(), expected_len);
        assert!(
            short_decoded
                .layers_received()
                .iter()
                .all(|&l| l < total_layers),
            "short prefix discard={discard} should have fewer than {total_layers} layers: {:?}",
            short_decoded.layers_received()
        );
    }
}
