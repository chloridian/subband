use crate::*;

#[test]
fn rct() {
    check("mct_rct", reference("main_rgb"));
}

#[test]
fn rct_four_components() {
    check("mct_rct_four_components", reference("main_rgba"));
}

#[test]
fn signed_components() {
    check("mct_signed_components", reference("main_rgb"));
}

#[test]
fn one_component_ignored() {
    check("mct_one_component_ignored", reference("main_r"));
}

#[test]
fn ict() {
    check("mct_ict", reference("main_rgb").tolerance(3));
}
