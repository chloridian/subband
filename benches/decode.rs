use divan::{Bencher, black_box};
use subband::{DecodeSettings, DecoderContext, Image};

use crate::{FIXTURES, Fixture, LAYERED};

const DISCARD_LEVELS: &[u32] = &[0, 1, 2, 5];
const REUSED_LEVELS: &[u32] = &[0, 2];
const PARTIAL_SIZES: &[usize] = &[600, 2000, 8192];

#[divan::bench(args = FIXTURES, consts = DISCARD_LEVELS)]
fn discard<const DISCARD: u32>(bencher: Bencher<'_, '_>, fixture: &Fixture) {
    run(bencher, fixture.bytes, true, DISCARD);
}

#[divan::bench(args = FIXTURES, consts = REUSED_LEVELS)]
fn reused<const DISCARD: u32>(bencher: Bencher<'_, '_>, fixture: &Fixture) {
    let settings = settings(fixture.bytes, true, DISCARD);
    let image = Image::from_codestream(fixture.bytes, &settings).expect("fixture parses");
    let mut ctx = DecoderContext::default();
    bencher.bench_local(|| {
        let decoded = image.decode(&mut ctx).expect("fixture decodes");
        black_box(decoded.data_u8().expect("not cancelled"));
    });
}

#[divan::bench(args = LAYERED, consts = PARTIAL_SIZES)]
fn partial<const BYTES: usize>(bencher: Bencher<'_, '_>, fixture: &Fixture) {
    let bytes = fixture.bytes;
    run(bencher, bytes.get(..BYTES).unwrap_or(bytes), false, 2);
}

fn run(bencher: Bencher<'_, '_>, data: &[u8], strict: bool, discard: u32) {
    let settings = settings(data, strict, discard);
    bencher.bench_local(|| {
        let image = Image::from_codestream(black_box(data), &settings).expect("fixture parses");
        let mut ctx = DecoderContext::default();
        let decoded = image.decode(&mut ctx).expect("fixture decodes");
        black_box(decoded.data_u8().expect("not cancelled"));
    });
}

fn settings(data: &[u8], strict: bool, discard: u32) -> DecodeSettings {
    let image = Image::from_codestream(data, &DecodeSettings::default()).expect("fixture parses");
    DecodeSettings {
        strict,
        target_resolution: Some((
            image.width().div_ceil(1 << discard),
            image.height().div_ceil(1 << discard),
        )),
        ..DecodeSettings::default()
    }
}
