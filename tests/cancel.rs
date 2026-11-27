use std::assert_matches;
use std::cell::Cell;

use subband::{Decode, DecodeError, DecodeSettings, Image};

use crate::*;

#[test]
fn cancelled() {
    let data = read_stream("code_blocks_all_styles");
    let image = Image::new(&data, &DecodeSettings::default()).expect("stream parses");
    let request = Decode::default();
    let mut buf = vec![0; image.output_len(&request)];
    assert_matches!(
        image.decode_into(&request, &mut buf, || true),
        Err(DecodeError::Cancelled)
    );
}

#[test]
fn cancelled_mid_decode() {
    let data = read_stream("code_blocks_all_styles");
    let image = Image::new(&data, &DecodeSettings::default()).expect("stream parses");
    let request = Decode::default();
    let mut buf = vec![0; image.output_len(&request)];
    let checks = Cell::new(0_usize);
    assert_matches!(
        image.decode_into(&request, &mut buf, || {
            let n = checks.get();
            checks.set(n + 1);
            n >= 4
        }),
        Err(DecodeError::Cancelled)
    );
}
