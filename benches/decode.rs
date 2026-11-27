use divan::{Bencher, black_box};
use subband::{Decode, DecodeSettings, Image};

use crate::{FIXTURES, Fixture, LAYERED};

const DISCARD_LEVELS: &[u8] = &[0, 1, 2, 5];
const PARTIAL_SIZES: &[usize] = &[600, 2000, 8192];

#[divan::bench(args = FIXTURES, consts = DISCARD_LEVELS)]
fn discard<const DISCARD: u8>(bencher: Bencher<'_, '_>, fixture: &Fixture) {
    run(bencher, fixture.bytes, true, DISCARD);
}

#[divan::bench(args = LAYERED, consts = PARTIAL_SIZES)]
fn partial<const BYTES: usize>(bencher: Bencher<'_, '_>, fixture: &Fixture) {
    let bytes = fixture.bytes;
    run(bencher, bytes.get(..BYTES).unwrap_or(bytes), false, 2);
}

fn run(bencher: Bencher<'_, '_>, data: &[u8], strict: bool, discard: u8) {
    let settings = DecodeSettings {
        strict,
        ..DecodeSettings::default()
    };
    let request = Decode {
        discard,
        max_layers: None,
    };
    bencher.bench_local(|| {
        let image = Image::from_codestream(black_box(data), &settings).expect("fixture parses");
        let mut buf = vec![0; image.output_len(&request)];
        let _ = image
            .decode_into(&request, &mut buf, || false)
            .expect("fixture decodes");
        black_box(buf);
    });
}
