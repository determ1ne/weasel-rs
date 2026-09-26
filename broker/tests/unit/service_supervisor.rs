use super::*;

#[test]
fn absent_child_shutdown_needs_no_rpc() {
    assert!(shutdown_component(&mut None, "unused-test-pipe".into(), "test").is_ok());
}
