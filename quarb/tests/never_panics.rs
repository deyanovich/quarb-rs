//! Malformed or outsized input refuses; it never panics, aborts or
//! runs away (the code review of 2026-11-10: one short query each
//! crashed the process). Every case here once did.

use quarb::Defs;
use quarb::expand;

fn refuse(q: &str) -> String {
    expand(q, &Defs::default()).unwrap_err().to_string()
}

/// A node-id anchor that opens and does not close: the anchor's
/// error was swallowed and the operand arm panicked ("lookahead
/// hit"); in branch position it was reported as a branch with no
/// step.
#[test]
fn a_malformed_node_id_anchor_refuses() {
    assert!(
        refuse("= ((!1)").contains("to close the node-id anchor"),
        "{}",
        refuse("= ((!1)")
    );
    for q in ["= ((!", "/a[((!))]", "= 1 + ((!", "= ((!abc", "((!1)/a"] {
        let _ = refuse(q);
    }
}
