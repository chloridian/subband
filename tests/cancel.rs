use std::assert_matches;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use subband::{CancelHandle, DecodeError, DecodeSettings, DecoderContext, Image};

use crate::*;

#[test]
fn cancelled() {
    check_error(
        "code_blocks_all_styles",
        DecodeSettings {
            cancel: Some(CancelHandle::new(|| true)),
            ..Default::default()
        },
        "decoding was cancelled",
    );
}

#[test]
fn write_to_cancelled() {
    let flag = Arc::new(AtomicBool::new(false));
    let check = Arc::clone(&flag);
    let settings = DecodeSettings {
        cancel: Some(CancelHandle::new(move || check.load(Ordering::Relaxed))),
        ..DecodeSettings::default()
    };
    let data = read_stream("code_blocks_all_styles");
    let image = Image::new(&data, &settings).expect("stream parses");
    let mut ctx = DecoderContext::default();
    let decoded = image.decode(&mut ctx).expect("stream decodes");
    let mut buf = vec![0; decoded.buffer_len()];
    flag.store(true, Ordering::Relaxed);

    assert_matches!(decoded.write_to(&mut buf), Err(DecodeError::Cancelled));
}
