#![no_main]

use libfuzzer_sys::fuzz_target;
use subband::{DecodeSettings, DecoderContext, Image};

fn run(data: &[u8], strict: bool) {
    let settings = DecodeSettings {
        strict,
        target_resolution: Some((512, 512)),
        ..DecodeSettings::default()
    };
    let Ok(image) = Image::from_codestream(data, 3, &settings) else {
        return;
    };
    if image.width() > 4096 || image.height() > 4096 {
        return;
    }
    let mut ctx = DecoderContext::default();
    if let Ok(decoded) = image.decode(&mut ctx) {
        let _ = decoded.data_u8();
    }
}

fuzz_target!(|data: &[u8]| {
    run(data, true);
    run(data, false);
    let Some(&[low, high]) = data.last_chunk::<2>() else {
        return;
    };
    let cut = usize::from(u16::from_le_bytes([low, high])) % data.len();
    run(&data[..cut], false);
});
