use core::fmt;

mod decode;
mod header;

macro_rules! fixture {
    ($name:literal) => {
        Fixture {
            name: $name,
            bytes: include_bytes!(concat!("fixtures/", $name, ".j2c")),
        }
    };
}

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

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

fn main() {
    divan::main();
}
