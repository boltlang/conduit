//! What the test programs in `tests/programs` can't check: which way
//! handlers are lowered, and that the output is deterministic.

use conduit_ir::text;

fn names(src: &str) -> String {
    let m = text::parse(src).unwrap();
    let bytes = crate::emit_module(&m, &[]).unwrap();
    wasmprinter::print_bytes(&bytes).unwrap()
}

/// Handlers whose clauses are all tail-resumptive or aborting are lowered
/// directly, without evidence passing's runtime.
#[test]
fn direct_handlers() {
    let wat = names(include_str!(
        "../../../tests/programs/handlers/state.conduit"
    ));
    assert!(!wat.contains("$lookup"), "{wat}");
    let wat = names(include_str!(
        "../../../tests/programs/handlers/aborting.conduit"
    ));
    assert!(!wat.contains("$lookup"), "{wat}");
    assert!(wat.contains("throw"), "{wat}");
}

/// A clause that does something after resuming needs evidence passing.
#[test]
fn general_handlers() {
    let wat = names(include_str!(
        "../../../tests/programs/handlers/not_tail_resumptive.conduit"
    ));
    assert!(wat.contains("$lookup"), "{wat}");
}

/// With stack switching, a general handler suspends instead.
#[test]
fn general_handlers_switching() {
    let m = text::parse(include_str!(
        "../../../tests/programs/handlers/not_tail_resumptive.conduit"
    ))
    .unwrap();
    let options = crate::Options {
        stack_switching: true,
    };
    let bytes = crate::emit_module_with(&m, &[], &options).unwrap();
    let wat = wasmprinter::print_bytes(&bytes).unwrap();
    assert!(!wat.contains("$lookup"), "{wat}");
    assert!(wat.contains("suspend"), "{wat}");
    assert!(wat.contains("cont.new"), "{wat}");
}

#[test]
fn output_is_deterministic() {
    let m = text::parse(include_str!("../../../tests/programs/data/lists.conduit")).unwrap();
    assert_eq!(crate::emit_module(&m, &[]), crate::emit_module(&m, &[]));
}
