//! The VDF module docs make two claims a reader can only check by grepping:
//! that a named test pins the on-chain cost of `verify_vdf`, and that
//! `MODULUS` is a 1024-bit group (issue #982).
//!
//! Both used to be false in a way no compiler could see. The cost paragraph
//! pointed at a test name and file that do not exist, which matters more than
//! a usual stale comment because the cost ceiling is the entire argument for
//! verifying a VDF on-chain — a pointer that cannot be grepped leaves a
//! reviewer unable to tell whether the budget constants are still enforced.
//!
//! `src/vdf.rs` is `pub`-free (`mod vdf;` at `src/lib.rs:34`), so the width of
//! `MODULUS` is not reachable from here; the in-module test in `vdf.rs` covers
//! the value, and this file covers the prose that describes it.
//!
//! The needles are assembled from fragments rather than written out: a
//! `grep -rn <stale name> contracts/refund-policy-vdf` is the acceptance check
//! for this issue, and a guard file that contains the string it forbids would
//! fail that check by itself.

/// The doc paragraph, read from the source it describes.
const VDF_SRC: &str = include_str!("../src/vdf.rs");

/// The crate's test module, which is where the pinned test has to actually live.
const TEST_SRC: &str = include_str!("../src/test.rs");

/// The file name the docs used to point at, which does not exist in the crate.
fn stale_file_name() -> String {
    ["vdf_", "test.rs"].concat()
}

/// The test name the docs used to point at.
fn stale_test_name() -> String {
    ["test_vdf_", "verify_resource_cost_budget"].concat()
}

/// The test that really pins the verification cost budget.
const BUDGET_TEST: &str = "test_verify_resource_cost_budget";

#[test]
fn the_docs_point_at_no_file_that_does_not_exist() {
    assert!(
        !VDF_SRC.contains(stale_file_name().as_str()),
        "the module docs reference `{}`, which is not a file in this crate",
        stale_file_name()
    );
    assert!(
        !VDF_SRC.contains(stale_test_name().as_str()),
        "the module docs reference the test `{}`, which does not exist",
        stale_test_name()
    );
}

#[test]
fn the_cost_claim_still_names_the_test_that_pins_it() {
    // The claim survives (issue #982: correct the pointer, do not delete it),
    // and the name it now gives resolves to a real test function.
    assert!(VDF_SRC.contains(BUDGET_TEST));
    assert!(
        TEST_SRC.contains(&format!("fn {BUDGET_TEST}(")),
        "`{BUDGET_TEST}` is cited by the docs but is not defined in `src/test.rs`"
    );
}

#[test]
fn the_modulus_is_documented_at_the_width_it_is_typed() {
    // `MODULUS` is typed `[u8; 128]`; the prose calls it 1024-bit. Both have to
    // stay in the file for the in-module width assertion to mean anything.
    assert!(VDF_SRC.contains("MODULUS: [u8; 128]"));
    assert!(VDF_SRC.contains("1024-bit"));
}

#[test]
fn the_cited_budget_constants_are_still_enforced() {
    // The paragraph's whole point: the ceilings are asserted, not merely
    // stated. If the test keeps its name but loses its comparisons, the pointer
    // is stale in a way a `contains` on the name cannot see.
    assert!(TEST_SRC.contains("VDF_VERIFY_MAX_CPU"));
    assert!(TEST_SRC.contains("VDF_VERIFY_MAX_MEM"));
    assert!(TEST_SRC.contains("cpu <= VDF_VERIFY_MAX_CPU"));
    assert!(TEST_SRC.contains("mem <= VDF_VERIFY_MAX_MEM"));
}
