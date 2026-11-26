#![no_main]

use libfuzzer_sys::fuzz_target;
use subband::{DecodeSettings, DecoderContext, Image};

fn run(data: &[u8], strict: bool) {
    let settings = DecodeSettings {
        strict,
        discard: 1,
        ..DecodeSettings::default()
    };
    if let Ok(info) = subband::inspect(data, &settings) {
        for d in 0..=info.levels().saturating_add(1) {
            let _ = info.bytes_for(d, None);
            let _ = info.bytes_for(d, Some(1));
        }
    }
    let Ok(image) = Image::from_codestream(data, &settings) else {
        return;
    };
    if image.width() > 4096 || image.height() > 4096 {
        return;
    }
    let mut ctx = DecoderContext::default();
    if let Ok(decoded) = image.decode(&mut ctx) {
        let _ = decoded.layers_received();
        let mut buf = vec![0; decoded.buffer_len()];
        let _ = decoded.write_to(&mut buf);
    }
}

fuzz_target!(|data: &[u8]| {
    run(data, true);
    run(data, false);

    let Some(&[low, high]) = data.last_chunk::<2>() else {
        return;
    };
    let cut = usize::from(u16::from_le_bytes([low, high])) % data.len();
    if let Some(prefix) = data.get(..cut) {
        run(prefix, true);
        run(prefix, false);
    }

    if let Some(&[idx_lo, idx_hi, mask]) = data.last_chunk::<3>()
        && mask != 0
    {
        let mut flipped = data.to_vec();
        let idx = usize::from(u16::from_le_bytes([idx_lo, idx_hi])) % flipped.len();
        if let Some(byte) = flipped.get_mut(idx) {
            *byte ^= mask;
        }
        run(&flipped, true);
        run(&flipped, false);
    }
});
