//! Every test program parses, prints back to the same module, and passes
//! the verifier, unless it has `verify-error` checks: then the verifier
//! rejects it with those messages.

use conduit_ir::text;
use conduit_ir::verify::verify_module;
use conduit_test::{CheckKind, Program};

fn main() {
    conduit_test::main(|_| true, test)
}

fn test(p: &Program) -> Result<(), String> {
    let m = p.module()?;
    let printed = text::print(&m);
    match text::parse(&printed) {
        Ok(again) if again == m => {}
        Ok(_) => {
            return Err(format!(
                "printing and parsing changes the module:\n{printed}"
            ));
        }
        Err(e) => return Err(format!("the printed module doesn't parse: {e}\n{printed}")),
    }

    let expected = p.messages(|k| match k {
        CheckKind::VerifyError(m) => Some(m),
        _ => None,
    });
    let errors: Vec<String> = match verify_module(&m) {
        Ok(()) => Vec::new(),
        Err(errors) => errors.iter().map(|e| e.named(&m)).collect(),
    };
    if expected.is_empty() && !errors.is_empty() {
        return Err(format!("the verifier rejects it:\n{}", errors.join("\n")));
    }
    for message in expected {
        if !errors.iter().any(|e| e.contains(message)) {
            return Err(format!(
                "no verifier error contains {message:?}. The errors are:\n{}",
                errors.join("\n")
            ));
        }
    }
    Ok(())
}
