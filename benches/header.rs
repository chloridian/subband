use divan::black_box;
use subband::{DecodeSettings, Image};

use crate::{FIXTURES, Fixture};

#[divan::bench(args = FIXTURES)]
fn parse(fixture: &Fixture) {
    let settings = DecodeSettings::default();
    black_box(Image::from_codestream(black_box(fixture.bytes), &settings).expect("fixture parses"));
}

#[divan::bench(args = FIXTURES)]
fn inspect(fixture: &Fixture) {
    let settings = DecodeSettings::default();
    black_box(
        subband::inspect(black_box(fixture.bytes), &settings, None).expect("fixture inspects"),
    );
}
