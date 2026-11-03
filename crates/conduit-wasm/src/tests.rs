//! What the test programs in `tests/programs` can't check. The programs
//! check what the lowering does (see `tests/programs.rs`).

use conduit_ir::text::parse;

use crate::emit_module;

#[test]
fn output_is_deterministic() {
    let m = parse(include_str!("../../../tests/programs/data/lists.conduit")).unwrap();
    assert_eq!(emit_module(&m, &[]), emit_module(&m, &[]));
}
