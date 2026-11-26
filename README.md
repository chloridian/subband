# Subband

Subband is a pure-Rust JPEG 2000 decoder for Second Life textures, forked from [hayro-jpeg2000](https://github.com/LaurenzV/hayro). It decodes raw J2C codestreams from untrusted sources at reduced resolution, supports cancellation, and unwraps the JP2 container format to decode its codestream.

## Example

```rust,no_run
use subband::{DecodeSettings, DecoderContext, Image};

let data = std::fs::read("texture.j2c").unwrap();
let settings = DecodeSettings {
    strict: true,
    target_resolution: Some((512, 512)),
    ..DecodeSettings::default()
};
let image = Image::from_codestream(&data, &settings).unwrap();

let mut ctx = DecoderContext::default();
let decoded = image.decode(&mut ctx).unwrap();
let pixels = decoded.data_u8().unwrap();
```

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.
