use std::assert_matches;
use std::fs;
use std::path::PathBuf;

use subband::{CancelHandle, DecodeError, DecodeSettings, DecoderContext, Image};

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
    let needed = subband::inspect(header, &DecodeSettings::default())
        .expect("header inspects")
        .bytes_for(2, None)
        .expect("discard 2 has a known length");
    let prefix = data.get(..needed).expect("fixture holds discard 2");

    let info = subband::inspect(prefix, &DecodeSettings::default()).expect("prefix inspects");
    let discard = info
        .complete_discard()
        .expect("prefix completes a discard level");
    assert!(discard <= 2, "discard {discard}");

    let settings = DecodeSettings {
        discard,
        ..DecodeSettings::default()
    };
    let image = Image::from_codestream(prefix, &settings).expect("prefix parses");
    assert_eq!(image.components(), info.components());

    let mut ctx = DecoderContext::default();
    let decoded = image.decode(&mut ctx).expect("prefix decodes");

    let expected_w = info.width().div_ceil(1 << discard);
    let expected_h = info.height().div_ceil(1 << discard);
    let expected_c = info.components();
    let expected_len = (expected_w as usize) * (expected_h as usize) * usize::from(expected_c);

    assert_eq!(decoded.width(), expected_w);
    assert_eq!(decoded.height(), expected_h);
    assert_eq!(decoded.components(), expected_c);
    assert_eq!(decoded.buffer_len(), expected_len);

    let mut buf = vec![0; decoded.buffer_len()];
    decoded.write_to(&mut buf).expect("write_to succeeds");
}

#[test]
fn cancelled_decode_returns_cancelled() {
    let data = fixture("rgba-1024-lrcp");
    let settings = DecodeSettings {
        cancel: Some(CancelHandle::new(|| true)),
        ..DecodeSettings::default()
    };
    let image = Image::from_codestream(&data, &settings).expect("codestream parses");
    let mut ctx = DecoderContext::default();
    assert_matches!(image.decode(&mut ctx).err(), Some(DecodeError::Cancelled));
}
