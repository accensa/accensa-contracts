//! #996 — pin `receipt_shard::BatchRecord`'s wire shape against the router's
//! independent copy.
//!
//! Cross-contract calls decode by shape (field names, types, order), not by
//! Rust type identity, so the shard and the router each declare their own
//! `BatchRecord`. Renaming a field, changing a type, reordering fields or
//! resizing `root` on one side alone would still compile while silently
//! breaking every `get_batch` / `verify_receipt` call in production.
//!
//! The checked-in fixture is the XDR (`ScVal`) encoding of one record,
//! generated once and treated as immutable: both structs must decode it into
//! the expected values and re-encode it byte-for-byte.

#![cfg(test)]

use receipt_anchor::BatchRecord as RouterBatchRecord;
use receipt_shard::BatchRecord as ShardBatchRecord;
use soroban_sdk::{
    xdr::{FromXdr, ToXdr},
    Bytes, BytesN, Env,
};

/// Committed wire fixture; see the file header for its provenance and the
/// values it encodes.
const FIXTURE: &str = include_str!("fixtures/batch_record_v1.hex");

const EXPECTED_COUNT: u32 = 7;
const EXPECTED_PERIOD_START: u64 = 1_000;
const EXPECTED_PERIOD_END: u64 = 2_000;
const EXPECTED_ANCHORED_LEDGER: u32 = 12_345;

fn fixture_bytes(env: &Env) -> Bytes {
    let hex: String = FIXTURE
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    assert!(!hex.is_empty(), "fixture carries no hex payload");
    assert_eq!(hex.len() % 2, 0, "hex payload must contain whole bytes");

    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("fixture must be valid hex"))
        .collect();
    Bytes::from_slice(env, &bytes)
}

#[test]
fn the_committed_fixture_decodes_into_both_batch_record_types() {
    let env = Env::default();
    let bytes = fixture_bytes(&env);
    let expected_root = BytesN::from_array(&env, &[0x11; 32]);

    let shard = ShardBatchRecord::from_xdr(&env, &bytes)
        .expect("receipt_shard::BatchRecord must decode the committed fixture");
    let router = RouterBatchRecord::from_xdr(&env, &bytes)
        .expect("receipt_anchor::BatchRecord must decode the committed fixture");

    assert_eq!(shard.root, expected_root);
    assert_eq!(shard.count, EXPECTED_COUNT);
    assert_eq!(shard.period_start, EXPECTED_PERIOD_START);
    assert_eq!(shard.period_end, EXPECTED_PERIOD_END);
    assert_eq!(shard.anchored_ledger, EXPECTED_ANCHORED_LEDGER);

    // The two independently defined structs agree field-for-field.
    assert_eq!(shard.root, router.root);
    assert_eq!(shard.count, router.count);
    assert_eq!(shard.period_start, router.period_start);
    assert_eq!(shard.period_end, router.period_end);
    assert_eq!(shard.anchored_ledger, router.anchored_ledger);
}

#[test]
fn both_types_re_encode_to_the_committed_bytes() {
    let env = Env::default();
    let bytes = fixture_bytes(&env);

    let shard = ShardBatchRecord::from_xdr(&env, &bytes).expect("shard record decodes");
    let router = RouterBatchRecord::from_xdr(&env, &bytes).expect("router record decodes");

    assert_eq!(
        shard.to_xdr(&env),
        bytes,
        "receipt_shard::BatchRecord re-encoded differently from the committed fixture"
    );
    assert_eq!(
        router.to_xdr(&env),
        bytes,
        "receipt_anchor::BatchRecord re-encoded differently from the committed fixture"
    );
}

/// One-shot generator for the committed fixture. Run it explicitly after a
/// deliberate wire-shape change and paste the output below the fixture's
/// header:
///
/// ```text
/// cargo test -p receipt-anchor --test shard_wire_shape_test -- --ignored --nocapture
/// ```
#[test]
#[ignore = "one-shot fixture generator; run with --ignored to print the hex"]
fn print_fixture_hex() {
    let env = Env::default();
    let record = ShardBatchRecord {
        root: BytesN::from_array(&env, &[0x11; 32]),
        count: EXPECTED_COUNT,
        period_start: EXPECTED_PERIOD_START,
        period_end: EXPECTED_PERIOD_END,
        anchored_ledger: EXPECTED_ANCHORED_LEDGER,
    };

    let hex: String = record
        .to_xdr(&env)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    println!("{hex}");
}
