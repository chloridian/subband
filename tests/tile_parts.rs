use crate::*;

#[test]
fn split_resolution() {
    check("tile_parts_split_resolution", reference("main_r"));
}

#[test]
fn split_layer() {
    check("tile_parts_split_layer", reference("main_r"));
}

#[test]
fn split_component() {
    check("tile_parts_split_component", reference("main_rgb"));
}

#[test]
fn interleaved() {
    check("tile_parts_interleaved", reference("main_r"));
}

#[test]
fn tnsot_zero() {
    check("tile_parts_tnsot_zero", reference("main_r"));
}

#[test]
fn tnsot_too_small() {
    check("tile_parts_tnsot_too_small", reference("main_r"));
}

#[test]
fn index_overflow() {
    check("tile_parts_index_overflow", reference("main_r"));
}

#[test]
fn psot_zero() {
    check("tile_parts_psot_zero", reference("main_r"));
}

#[test]
fn psot_zero_padding() {
    check("tile_parts_psot_zero_padding", reference("main_r"));
}

#[test]
fn missing_eoc() {
    check("tile_parts_missing_eoc", reference("main_r"));
}

#[test]
fn bytes_after_eoc() {
    check("tile_parts_bytes_after_eoc", reference("main_r"));
}

#[test]
fn garbage_before_eoc() {
    check("tile_parts_garbage_before_eoc", reference("main_r"));
}

#[test]
fn truncated_end() {
    check("tile_parts_truncated_end", reference("main_r"));
}

#[test]
fn trailing_packet_data() {
    check("tile_parts_trailing_packet_data", reference("main_r"));
}

#[test]
fn truncated_layers() {
    check(
        "tile_parts_truncated_layers",
        reference("tile_parts_truncated_layers"),
    );
}

#[test]
fn sot_only_tile_part() {
    check("tile_parts_sot_only_tile_part", reference("main_r"));
}

#[test]
fn cod_qcd_in_later_tile_part() {
    check("tile_parts_cod_qcd_in_later_tile_part", reference("main_r"));
}

#[test]
fn truncated_in_packet_header() {
    let full = reference("main_r");
    for strict in [false, true] {
        let settings = DecodeSettings {
            strict,
            ..DecodeSettings::default()
        };
        let actual = decode("tile_parts_truncated_in_packet_header", &settings)
            .unwrap_or_else(|e| panic!("strict={strict}: {e}"));
        assert_eq!((actual.width, actual.height), (full.width, full.height));
        assert_eq!(actual.channels, full.channels);
        for y in 0..full.height as usize {
            for x in 0..full.width as usize {
                if y < 48 || x < 64 {
                    let i = y * full.width as usize + x;
                    assert_eq!(
                        actual.samples[i], full.samples[i],
                        "strict={strict} ({x},{y})"
                    );
                }
            }
        }
    }
}

#[test]
fn truncated_in_packet_body() {
    let full = reference("main_r");
    for strict in [false, true] {
        let settings = DecodeSettings {
            strict,
            ..DecodeSettings::default()
        };
        let actual = decode("tile_parts_truncated_in_packet_body", &settings)
            .unwrap_or_else(|e| panic!("strict={strict}: {e}"));
        assert_eq!((actual.width, actual.height), (full.width, full.height));
        assert_eq!(actual.channels, full.channels);
        for y in 0..full.height as usize {
            for x in 0..full.width as usize {
                if y < 48 || x < 64 {
                    let i = y * full.width as usize + x;
                    assert_eq!(
                        actual.samples[i], full.samples[i],
                        "strict={strict} ({x},{y})"
                    );
                }
            }
        }
    }
}
