use super::public_op_name;

#[test]
fn public_op_name_format() {
    assert_eq!(public_op_name("generic", "hover"), "plugin.generic.hover");
}
