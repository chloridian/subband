use crate::*;

#[test]
fn irreversible() {
    check(
        "quantization_irreversible",
        reference("main_r").tolerance(2),
    );
}

#[test]
fn scalar_derived() {
    check(
        "quantization_scalar_derived",
        reference("main_r").tolerance(1),
    );
}

#[test]
fn irreversible_tiny_tiles() {
    check(
        "quantization_irreversible_tiny_tiles",
        reference("main_rgb_crop_50_40_12_12").tolerance(2),
    );
}

#[test]
fn lossy_layers() {
    check(
        "quantization_lossy_layers",
        reference("quantization_lossy_layers").tolerance(1),
    );
}

#[test]
fn irreversible_rate() {
    check(
        "quantization_irreversible_rate",
        reference("quantization_irreversible_rate").tolerance(1),
    );
}

#[test]
fn reversible_lossy_layers() {
    check(
        "quantization_reversible_lossy_layers",
        reference("quantization_reversible_lossy_layers"),
    );
}
