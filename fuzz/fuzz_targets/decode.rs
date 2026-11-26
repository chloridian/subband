#![no_main]

use libfuzzer_sys::fuzz_target;
use subband::{DecodeSettings, DecoderContext, Image};

fuzz_target!(|data: &[u8]| {
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
});
