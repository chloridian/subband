use core::cell::RefCell;

use divan::{Bencher, black_box};
use subband::{DecodeSettings, DecoderContext, Image};

use crate::{FIXTURES, Fixture, LAYERED};

const DISCARD_LEVELS: &[u8] = &[0, 1, 2, 5];
const REUSED_LEVELS: &[u8] = &[0, 2];
const PARTIAL_SIZES: &[usize] = &[600, 2000, 8192];

thread_local! {
    static SHARED: RefCell<SharedState> = RefCell::new(SharedState::default());
}

#[derive(Default)]
struct SharedState {
    ctx: DecoderContext,
    buf: Vec<u8>,
    warmed: bool,
}

impl SharedState {
    fn warm(&mut self) {
        if self.warmed {
            return;
        }
        let settings = settings(true, 0);
        let mut max_len = 0;
        for fixture in FIXTURES {
            let image = Image::from_codestream(fixture.bytes, &settings).expect("fixture parses");
            let decoded = image.decode(&mut self.ctx).expect("fixture decodes");
            max_len = max_len.max(decoded.buffer_len());
        }
        self.buf.resize(max_len, 0);
        self.warmed = true;
    }
}

#[divan::bench(args = FIXTURES, consts = DISCARD_LEVELS)]
fn discard<const DISCARD: u8>(bencher: Bencher<'_, '_>, fixture: &Fixture) {
    run(bencher, fixture.bytes, true, DISCARD);
}

#[divan::bench(args = FIXTURES, consts = REUSED_LEVELS)]
fn reused<const DISCARD: u8>(bencher: Bencher<'_, '_>, fixture: &Fixture) {
    let settings = settings(true, DISCARD);
    let image = Image::from_codestream(fixture.bytes, &settings).expect("fixture parses");
    let mut ctx = DecoderContext::default();
    let len = image
        .decode(&mut ctx)
        .expect("fixture decodes")
        .buffer_len();
    let mut buf = vec![0; len];
    bencher.bench_local(|| {
        let decoded = image.decode(&mut ctx).expect("fixture decodes");
        decoded.write_to(&mut buf).expect("not cancelled");
        black_box(&buf);
    });
}

#[divan::bench(args = FIXTURES, consts = REUSED_LEVELS)]
fn shared<const DISCARD: u8>(bencher: Bencher<'_, '_>, fixture: &Fixture) {
    let settings = settings(true, DISCARD);
    let image = Image::from_codestream(fixture.bytes, &settings).expect("fixture parses");
    SHARED.with(|cell| {
        let mut state = cell.borrow_mut();
        state.warm();
        let SharedState { ctx, buf, .. } = &mut *state;
        bencher.bench_local(|| {
            let decoded = image.decode(ctx).expect("fixture decodes");
            let len = decoded.buffer_len();
            let out = buf.get_mut(..len).expect("shared buffer fits fixture");
            decoded.write_to(out).expect("not cancelled");
            black_box(out);
        });
    });
}

#[divan::bench(args = LAYERED, consts = PARTIAL_SIZES)]
fn partial<const BYTES: usize>(bencher: Bencher<'_, '_>, fixture: &Fixture) {
    let bytes = fixture.bytes;
    run(bencher, bytes.get(..BYTES).unwrap_or(bytes), false, 2);
}

fn run(bencher: Bencher<'_, '_>, data: &[u8], strict: bool, discard: u8) {
    let settings = settings(strict, discard);
    bencher.bench_local(|| {
        let image = Image::from_codestream(black_box(data), &settings).expect("fixture parses");
        let mut ctx = DecoderContext::default();
        let decoded = image.decode(&mut ctx).expect("fixture decodes");
        let mut buf = vec![0; decoded.buffer_len()];
        decoded.write_to(&mut buf).expect("not cancelled");
        black_box(buf);
    });
}

fn settings(strict: bool, discard: u8) -> DecodeSettings {
    DecodeSettings {
        strict,
        discard,
        ..DecodeSettings::default()
    }
}
