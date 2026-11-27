Subband is a pure-Rust JPEG 2000 decoder for Second Life textures, forked from [hayro-jpeg2000](https://github.com/LaurenzV/hayro). It decodes raw J2C codestreams from untrusted sources at reduced resolution, supports cancellation, and unwraps the JP2 container format to decode its codestream.

## Example

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use subband::{CancelHandle, DecodeSettings, DecoderContext, Image};

fn main() -> Result<(), subband::DecodeError> {
    let data = include_bytes!("benches/fixtures/plywood.j2c");
    let prefix = &data[..4096];

    let info = subband::inspect(prefix, &DecodeSettings::default())?;
    let discard = info.complete_discard().unwrap_or(info.levels());

    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancelled);
    let settings = DecodeSettings {
        discard,
        cancel: Some(CancelHandle::new(move || flag.load(Ordering::Relaxed))),
        ..DecodeSettings::default()
    };

    let image = Image::from_codestream(prefix, &settings)?;
    let mut ctx = DecoderContext::default();
    let decoded = image.decode(&mut ctx)?;

    let mut pixels = vec![0; decoded.buffer_len()];
    decoded.write_to(&mut pixels)?;
    let (_width, _height, _components) = (decoded.width(), decoded.height(), decoded.components());

    Ok(())
}
```

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.
