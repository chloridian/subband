Subband is a pure-Rust JPEG 2000 decoder for Second Life textures, forked from [hayro-jpeg2000](https://github.com/LaurenzV/hayro). It decodes raw J2C codestreams from untrusted sources at reduced resolution, supports cancellation, and unwraps the JP2 container format to decode its codestream.

## Example

```rust
use std::sync::atomic::{AtomicBool, Ordering};

use subband::{Decode, DecodeSettings, Image};

fn main() -> Result<(), subband::DecodeError> {
    let data = include_bytes!("benches/fixtures/plywood.j2c");
    let prefix = &data[..4096];

    let settings = DecodeSettings::default();
    let info = subband::inspect(prefix, &settings, None)?;
    let discard = info.complete_discard().unwrap_or(info.levels());

    let cancelled = AtomicBool::new(false);
    let image = Image::from_codestream(prefix, &settings)?;
    let request = Decode {
        discard,
        max_layers: None,
    };
    let [_width, _height] = image.output_size(&request);
    let mut pixels = vec![0; image.output_len(&request)];
    let decoded = image.decode_into(&request, &mut pixels, || cancelled.load(Ordering::Relaxed))?;
    let _layers = decoded.layers_received();

    Ok(())
}
```

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.
