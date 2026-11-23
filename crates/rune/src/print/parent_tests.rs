#[test]
fn an_x11_parent_is_its_hex_window_id() {
    assert_eq!(super::x11(0), "");
    assert_eq!(super::x11(0x3a0000f), "x11:3a0000f");
}
