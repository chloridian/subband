#![no_main]

use libfuzzer_sys::fuzz_target;
use subband::{DecodeSettings, DecoderContext, Image};

fn run(data: &[u8]) {
    let settings = DecodeSettings {
        strict: true,
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
    run(data);
    if data.len() >= 2 {
        let cut = usize::from(u16::from_le_bytes([
            data[data.len() - 2],
            data[data.len() - 1],
        ])) % data.len();
        run(&data[..cut]);
    }
});
