// GENERATED FILE — do not edit by hand.
//
// Emitted by contracts/receipt-anchor/scripts/build-event-topic-vectors.mjs
// from the canonical contracts/receipt-anchor/event-topic-vectors.json (issue
// #942). That JSON file is the single source of truth for which topic tuple
// each contract version publishes; both contracts' `event_topic_versions`
// tests consume this fixture (the refund-vault test includes it by relative
// path), so a shape change anywhere fails one of the suites.
//
// Tag semantics: a bare word is a Symbol of that name, "u64" is a u64 scalar
// position, "bytes32" is a BytesN<32> position.
//
// Recorded discrepancies (named, not settled — no side was edited):
//   - anchor-receipt-topics-three-way-split [recorded, not settled: no side was edited by this issue]
//     code: anchor_batch and prune_batches publish (receipt, action, anchor_id) via the single publisher contracts/receipt-anchor/src/events.rs:95-99 (call sites src/lib.rs:549 and src/lib.rs:1220)
//     docs: docs/EVENTS.md:20-28 documents ("anchor_event", shard_id, batch_id); the AnchorEvent/PruneEvent #[contractevent] structs at contracts/receipt-anchor/src/lib.rs:166-204 match the docs but are never constructed or published
//     snapshots: five committed snapshots still record the two-element ("anchor_event", batch_id), e.g. contracts/receipt-anchor/test_snapshots/test/test_first_anchor_always_succeeds.1.json:492
//
// To regenerate locally:
//   node contracts/receipt-anchor/scripts/build-event-topic-vectors.mjs

// receipt-anchor :: anchor
/// Topics (namespace, action, batch_id). Provenance: contracts/receipt-anchor/src/events.rs:95-99 publishes (Symbol("receipt"), action.topic(env), anchor_id); ReceiptAnchor::anchor_batch calls it with ReceiptAction::Anchor and batch_id (src/lib.rs:549)
#[allow(dead_code)]
pub const ANCHOR_CURRENT: &[&str] = &["receipt", "anchor", "u64"];

/// Topics (legacy_symbol, batch_id). Provenance: DEPLOYMENTS.md:29 and CHANGELOG.md:1906 record 0.1.0 publishing ("anchored", batch_id) by hand; docs/AUDIT.md:86 repeats the old topics
#[allow(dead_code)]
pub const ANCHOR_LEGACY_0_1_0: Option<&[&str]> = Some(&["anchored", "u64"]);

// receipt-anchor :: prune
/// Topics (namespace, action, start_batch_id). Provenance: contracts/receipt-anchor/src/events.rs:95-99 publishes (Symbol("receipt"), action.topic(env), anchor_id); ReceiptAnchor::prune_batches calls it with ReceiptAction::Prune and start_batch_id (src/lib.rs:1220)
#[allow(dead_code)]
pub const PRUNE_CURRENT: &[&str] = &["receipt", "prune", "u64"];

/// No legacy_0_1_0 arm: prune_batches did not exist in 0.1.0 (DEPLOYMENTS.md:25-28), so no legacy prune shape was ever published.
#[allow(dead_code)]
pub const PRUNE_LEGACY_0_1_0: Option<&[&str]> = None;

// refund-vault :: refund
/// Topics (event_symbol, payment_ref). Provenance: contracts/refund-vault/src/lib.rs:298-302 derives #[contractevent] with #[topic] payment_ref, documented as ("refund_event", payment_ref) at src/lib.rs:290; agrees with docs/EVENTS.md:150
#[allow(dead_code)]
pub const REFUND_CURRENT: &[&str] = &["refund_event", "bytes32"];

/// Topics (legacy_symbol, payment_ref). Provenance: DEPLOYMENTS.md:29 and CHANGELOG.md:1906 record 0.1.0 publishing ("refunded", payment_ref) by hand; docs/AUDIT.md:86 repeats the old topics
#[allow(dead_code)]
pub const REFUND_LEGACY_0_1_0: Option<&[&str]> = Some(&["refunded", "bytes32"]);

// refund-vault :: batch_refund
/// Topics (event_symbol). Provenance: contracts/refund-vault/src/lib.rs:321-329 derives #[contractevent] with no #[topic] field, documented as ("batch_refund_event",) at src/lib.rs:320; emitted once per process_batch call
#[allow(dead_code)]
pub const BATCH_REFUND_CURRENT: &[&str] = &["batch_refund_event"];

/// No legacy_0_1_0 arm: the surviving 0.1.0 sources name only the ("anchored", ...) and ("refunded", ...) shapes (DEPLOYMENTS.md:29), so no legacy batch shape is pinned.
#[allow(dead_code)]
pub const BATCH_REFUND_LEGACY_0_1_0: Option<&[&str]> = None;
