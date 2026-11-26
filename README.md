<!-- cargo-rdme start -->

Subband is a fork of [hayro-jpeg2000](https://github.com/LaurenzV/hayro), tuned for the
textures Second Life serves: raw JPEG 2000 codestreams (`.j2c`) from untrusted sources,
decoded at reduced resolution and cancelled when no longer needed. It also unwraps the
JP2 container format to decode its codestream. An encoder is planned.

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

## Testing

Before the fork, hayro-jpeg2000 was tested against more than 20,000 images taken from
PDFs, and it passes a large part of the `OpenJPEG` test suite.

## Safety

By default, the crate has the `simd` feature enabled, which uses the
[`fearless_simd`](https://github.com/linebender/fearless_simd) crate to accelerate
important parts of the pipeline. If you want to eliminate any usage of unsafe
in this crate as well as its dependencies, you can simply disable this
feature, at the cost of slower decoding. Unsafe code is forbidden
via a crate-level attribute.

The crate is `no_std` compatible but requires an allocator to be available.

<!-- cargo-rdme end -->

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.
