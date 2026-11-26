//! Pin the proof-length check's precedence at both receipt-anchor entry points.
//!
//! This table is intentionally unresolved-state-aware: length is checked before
//! the batch/shard lookup in the batch-id path, while the by-root path applies
//! its router-side length guard before scanning the per-shard root buffer.

use accensa_common::Error;
use receipt_anchor::{ReceiptAnchor, ReceiptAnchorClient};
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env, Vec};

mod shard_wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/receipt_shard.wasm");
}

const SHARD: u64 = 7;
const MAX_PROOF_LEN: u32 = 10;

fn setup() -> (Env, ReceiptAnchorClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let anchor_id = env.register(ReceiptAnchor, ());
    let anchor = ReceiptAnchorClient::new(&env, &anchor_id);
    let shard_wasm_hash = env.deployer().upload_contract_wasm(shard_wasm::WASM);
    anchor.initialize(&merchant, &shard_wasm_hash);

    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    anchor.set_bounty_asset(&token);

    (env, anchor)
}

fn siblings(env: &Env, len: u32) -> Vec<BytesN<32>> {
    let mut proof = Vec::new(env);
    for i in 0..len {
        proof.push_back(BytesN::from_array(env, &[i as u8; 32]));
    }
    proof
}

fn bytes(env: &Env, byte: u8) -> BytesN<32> {
    BytesN::from_array(env, &[byte; 32])
}

#[derive(Clone, Copy, Debug)]
struct BoundaryRow {
    proof_len: u32,
    existing_batch: bool,
    known_root: bool,
    expected_batch: Option<Result<bool, Error>>,
    expected_root: Option<Result<bool, Error>>,
}

#[test]
fn proof_length_boundary_table_pins_result_precedence() {
    let (env, anchor) = setup();

    let leaf = bytes(&env, 0x42);
    let known_root = bytes(&env, 0x21);
    let unknown_root = bytes(&env, 0x99);

    let existing_batch_id = anchor.anchor_batch(&SHARD, &known_root, &1, &0, &10);

    let rows = [
        // Existing-batch / known-root paths do not return ProofTooLong at 10.
        BoundaryRow {
            proof_len: 10,
            existing_batch: true,
            known_root: true,
            expected_batch: Some(Ok(false)),
            expected_root: Some(Ok(false)),
        },
        BoundaryRow {
            proof_len: 11,
            existing_batch: true,
            known_root: true,
            expected_batch: Some(Err(Error::ProofTooLong)),
            expected_root: Some(Err(Error::ProofTooLong)),
        },
        // Missing batch_id reaches the shard lookup with an in-bound proof,
        // but an 11-node proof is rejected before that lookup.
        BoundaryRow {
            proof_len: 10,
            existing_batch: false,
            known_root: false,
            expected_batch: Some(Err(Error::BatchNotFound)),
            expected_root: Some(Err(Error::RootNotFound)),
        },
        BoundaryRow {
            proof_len: 11,
            existing_batch: false,
            known_root: false,
            expected_batch: Some(Err(Error::ProofTooLong)),
            expected_root: Some(Err(Error::ProofTooLong)),
        },
    ];

    for row in rows {
        let proof = siblings(&env, row.proof_len);

        let batch_id = if row.existing_batch {
            existing_batch_id
        } else {
            0
        };
        let batch_result = anchor.try_verify_receipt(&SHARD, &batch_id, &leaf, &proof);
        match row.expected_batch {
            Some(Ok(false)) => assert_eq!(
                batch_result,
                Ok(Ok(false)),
                "10-node proof against an existing batch"
            ),
            Some(Err(Error::ProofTooLong)) => assert_eq!(
                batch_result,
                Err(Ok(Error::ProofTooLong)),
                "proof length {}/existing={} must win before any batch mismatch",
                row.proof_len,
                row.existing_batch
            ),
            Some(Err(Error::BatchNotFound)) => assert_eq!(
                batch_result,
                Err(Ok(Error::BatchNotFound)),
                "10-node proof with missing batch_id must reach lookup first"
            ),
            _ => unreachable!("invalid batch expectation"),
        }

        let root = if row.known_root {
            known_root.clone()
        } else {
            unknown_root.clone()
        };
        let root_result = anchor.try_verify_receipt_by_root(&SHARD, &root, &leaf, &proof);
        match row.expected_root {
            Some(Ok(false)) => assert_eq!(
                root_result,
                Ok(Ok(false)),
                "10-node proof against known root"
            ),
            Some(Err(Error::ProofTooLong)) => assert_eq!(
                root_result,
                Err(Ok(Error::ProofTooLong)),
                "{}-node proof against {} root must return ProofTooLong",
                row.proof_len,
                if row.known_root { "known" } else { "unknown" }
            ),
            Some(Err(Error::RootNotFound)) => assert_eq!(
                root_result,
                Err(Ok(Error::RootNotFound)),
                "10-node proof against unknown root must reach root scan"
            ),
            _ => unreachable!("invalid root expectation"),
        }
    }

    // Explicit batch_id=0 row records lookup-first precedence separately from
    // the known-batch table and guards against accidental router-side prechecks.
    assert_eq!(
        anchor.try_verify_receipt(&SHARD, &0, &leaf, &siblings(&env, MAX_PROOF_LEN)),
        Err(Ok(Error::BatchNotFound))
    );
    assert_eq!(
        anchor.try_verify_receipt(&SHARD, &0, &leaf, &siblings(&env, MAX_PROOF_LEN + 1)),
        Err(Ok(Error::ProofTooLong))
    );

    // Explicit unknown-root rows make the router's own guard-before-buffer-scan
    // order observable. This fails if check :583 is moved below the buffer scan.
    assert_eq!(
        anchor.try_verify_receipt_by_root(
            &SHARD,
            &unknown_root,
            &leaf,
            &siblings(&env, MAX_PROOF_LEN)
        ),
        Err(Ok(Error::RootNotFound))
    );
    assert_eq!(
        anchor.try_verify_receipt_by_root(
            &SHARD,
            &unknown_root,
            &leaf,
            &siblings(&env, MAX_PROOF_LEN + 1)
        ),
        Err(Ok(Error::ProofTooLong))
    );

    // Both entry points return false, not ProofTooLong, at the inclusive bound
    // when the supplied proof is validly shaped but wrong for the target.
    assert_eq!(
        anchor.try_verify_receipt(
            &SHARD,
            &existing_batch_id,
            &leaf,
            &siblings(&env, MAX_PROOF_LEN)
        ),
        Ok(Ok(false))
    );
    assert_eq!(
        anchor.try_verify_receipt_by_root(
            &SHARD,
            &known_root,
            &leaf,
            &siblings(&env, MAX_PROOF_LEN)
        ),
        Ok(Ok(false))
    );
}
