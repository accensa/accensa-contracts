//! `AnchorEventPayload`'s five public fields stay documented (issue #772).
//!
//! The runtime values are already pinned by
//! `contracts/receipt-anchor/src/test.rs::test_anchor_event_uses_canonical_topics`;
//! what that cannot see is whether the fields still carry the `///` lines an
//! off-chain decoder reads, so this asserts their presence directly. It reads
//! the source text rather than the compiled item because a missing comment is
//! not observable from a `#[contracttype]` value.

const EVENTS_SOURCE: &str = include_str!("../src/events.rs");

#[test]
fn anchor_event_payload_fields_have_documentation() {
    let payload_start = EVENTS_SOURCE
        .find("pub struct AnchorEventPayload {")
        .expect("AnchorEventPayload must exist");
    let payload_tail = &EVENTS_SOURCE[payload_start..];
    let payload_end = payload_tail
        .find("\n}")
        .expect("AnchorEventPayload must be closed");
    let payload = &payload_tail[..payload_end];

    for field in ["shard_id", "batch_id", "root", "count", "timestamp"] {
        let declaration = format!("pub {field}:");
        let (before_field, _) = payload
            .split_once(&declaration)
            .unwrap_or_else(|| panic!("AnchorEventPayload must contain {field}"));
        let doc_line = before_field
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_else(|| panic!("{field} must have a preceding doc comment"));

        assert!(
            doc_line.trim_start().starts_with("///"),
            "{field} must have a preceding doc comment"
        );
    }

    for field in ["root", "count"] {
        let declaration = format!("pub {field}:");
        let (before_field, _) = payload
            .split_once(&declaration)
            .unwrap_or_else(|| panic!("AnchorEventPayload must contain {field}"));
        let doc_line = before_field
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_else(|| panic!("{field} must have a preceding doc comment"));

        assert!(
            doc_line.contains("anchor_batch") && doc_line.contains("anchor_batch_zk"),
            "{field} documentation must describe both anchoring entry points"
        );
    }
}
