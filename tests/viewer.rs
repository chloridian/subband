use std::assert_matches;
use std::fs;
use std::path::PathBuf;

use subband::{Decode, DecodeError, DecodeSettings, Image};

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("benches")
        .join("fixtures")
        .join(format!("{name}.j2c"));
    fs::read(&path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

#[test]
fn prefix_decode_at_complete_discard() {
    let data = fixture("rgba-1024-lrcp");
    let header = data.get(..2000).expect("fixture is at least 2000 bytes");
    let settings = DecodeSettings::default();
    let needed = subband::inspect(header, &settings, None)
        .expect("header inspects")
        .bytes_for(2, None)
        .expect("discard 2 has a known length");
    let prefix = data.get(..needed).expect("fixture holds discard 2");

    let info = subband::inspect(prefix, &settings, None).expect("prefix inspects");
    let discard = info
        .complete_discard()
        .expect("prefix completes a discard level");
    assert!(discard <= 2, "discard {discard}");

    let image = Image::from_codestream(prefix, &settings).expect("prefix parses");
    assert_eq!(image.components(), info.components());

    let request = Decode {
        discard,
        max_layers: None,
    };
    let expected_w = info.width().div_ceil(1 << discard);
    let expected_h = info.height().div_ceil(1 << discard);
    let expected_c = info.components();
    let expected_len = (expected_w as usize) * (expected_h as usize) * usize::from(expected_c);

    assert_eq!(image.output_size(&request), [expected_w, expected_h]);
    assert_eq!(image.output_len(&request), expected_len);

    let mut buf = vec![0; expected_len];
    let decoded = image
        .decode_into(&request, &mut buf, || false)
        .expect("prefix decodes");

    assert_eq!(decoded.width(), expected_w);
    assert_eq!(decoded.height(), expected_h);
    assert_eq!(decoded.components(), expected_c);
}

#[test]
fn cancelled_decode_returns_cancelled() {
    let data = fixture("rgba-1024-lrcp");
    let image =
        Image::from_codestream(&data, &DecodeSettings::default()).expect("codestream parses");
    let request = Decode::default();
    let mut buf = vec![0; image.output_len(&request)];
    assert_matches!(
        image.decode_into(&request, &mut buf, || true),
        Err(DecodeError::Cancelled)
    );
}
