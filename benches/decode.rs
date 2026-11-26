use core::fmt;

use divan::{Bencher, black_box};
use subband::{DecodeSettings, DecoderContext, Image};

macro_rules! fixture {
    ($name:literal) => {
        Fixture {
            name: $name,
            bytes: include_bytes!(concat!("fixtures/", $name, ".j2c")),
        }
    };
}

const DISCARD_LEVELS: &[u32] = &[0, 1, 2, 5];
const PARTIAL_SIZES: &[usize] = &[600, 2000, 8192];

const FIXTURES: &[Fixture] = &[
    fixture!("plywood"),
    fixture!("terrain-grass"),
    fixture!("water-normal"),
    fixture!("profile-picture"),
    fixture!("alpha-gradient"),
    fixture!("white"),
    fixture!("gray-alpha"),
    fixture!("gray-4096"),
    fixture!("sculpt-64"),
    fixture!("rgb-1024-rlcp"),
    fixture!("rgba-1024x512-rlcp"),
    fixture!("rgba-1024-lrcp"),
    fixture!("rgba-2048-lossy"),
];

const LAYERED: &[Fixture] = &[
    fixture!("plywood"),
    fixture!("rgb-1024-rlcp"),
    fixture!("rgba-1024-lrcp"),
];

struct Fixture {
    name: &'static str,
    bytes: &'static [u8],
}

impl fmt::Display for Fixture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name)
    }
}

#[divan::bench(args = FIXTURES, consts = DISCARD_LEVELS)]
fn discard<const DISCARD: u32>(bencher: Bencher<'_, '_>, fixture: &Fixture) {
    run(bencher, fixture.bytes, true, DISCARD);
}

#[divan::bench(args = LAYERED, consts = PARTIAL_SIZES)]
fn partial<const BYTES: usize>(bencher: Bencher<'_, '_>, fixture: &Fixture) {
    let bytes = fixture.bytes;
    run(bencher, &bytes[..BYTES.min(bytes.len())], false, 2);
}

fn run(bencher: Bencher<'_, '_>, data: &[u8], strict: bool, discard: u32) {
    let image = Image::from_codestream(data, &DecodeSettings::default()).unwrap();
    let settings = DecodeSettings {
        strict,
        target_resolution: Some((
            image.width().div_ceil(1 << discard),
            image.height().div_ceil(1 << discard),
        )),
        cancel: None,
    };
    bencher.bench_local(|| {
        let image = Image::from_codestream(black_box(data), &settings).unwrap();
        let mut ctx = DecoderContext::default();
        black_box(image.decode(&mut ctx).unwrap().data_u8().unwrap());
    });
}
