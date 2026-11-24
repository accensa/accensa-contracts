//! Pin the proof-length check's precedence at both receipt-anchor entry points.

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

#[test]
fn proof_length_boundary_table_pins_result_precedence() {
    let (env, anchor) = setup();

    let leaf = bytes(&env, 0x42);
    let known_root = bytes(&env, 0x21);
    let unknown_root = bytes(&env, 0x99);

    let existing_batch_id =
        anchor.anchor_batch(&SHARD, &known_root, &1, &0, &10);

    // 10 nodes against an existing batch: in bounds.
    assert_eq!(
        anchor.try_verify_receipt(
            &SHARD,
            &existing_batch_id,
            &leaf,
            &siblings(&env, MAX_PROOF_LEN),
        ),
        Ok(Ok(false)),
    );

    // 11 nodes against an existing batch: ProofTooLong.
    assert_eq!(
        anchor.try_verify_receipt(
            &SHARD,
            &existing_batch_id,
            &leaf,
            &siblings(&env, MAX_PROOF_LEN + 1),
        ),
        Err(Ok(Error::ProofTooLong)),
    );

    // Batch lookup first: batch_id = 0 with an in-bound proof returns BatchNotFound.
    assert_eq!(
        anchor.try_verify_receipt(
            &SHARD,
            &0,
            &leaf,
            &siblings(&env, MAX_PROOF_LEN),
        ),
        Err(Ok(Error::BatchNotFound)),
    );

    // An over-bound proof wins before the batch lookup.
    assert_eq!(
        anchor.try_verify_receipt(
            &SHARD,
            &0,
            &leaf,
            &siblings(&env, MAX_PROOF_LEN + 1),
        ),
        Err(Ok(Error::ProofTooLong)),
    );

    // 10 nodes against an unknown root reach the root buffer and return RootNotFound.
    assert_eq!(
        anchor.try_verify_receipt_by_root(
            &SHARD,
            &unknown_root,
            &leaf,
            &siblings(&env, MAX_PROOF_LEN),
        ),
        Err(Ok(Error::RootNotFound)),
    );

    // The router's own 11-node guard wins before the unknown-root buffer scan.
    assert_eq!(
        anchor.try_verify_receipt_by_root(
            &SHARD,
            &unknown_root,
            &leaf,
            &siblings(&env, MAX_PROOF_LEN + 1),
        ),
        Err(Ok(Error::ProofTooLong)),
    );

    // A known root is matched by the buffer, then the proof shape determines the result.
    assert_eq!(
        anchor.try_verify_receipt_by_root(
            &SHARD,
            &known_root,
            &leaf,
            &siblings(&env, MAX_PROOF_LEN),
        ),
        Ok(Ok(false)),
    );

    assert_eq!(
        anchor.try_verify_receipt_by_root(
            &SHARD,
            &known_root,
            &leaf,
            &siblings(&env, MAX_PROOF_LEN + 1),
        ),
        Err(Ok(Error::ProofTooLong)),
    );
}
