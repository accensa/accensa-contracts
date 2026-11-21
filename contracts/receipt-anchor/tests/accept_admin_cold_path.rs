//! #956 — a rejected `accept_admin` has to be proven a no-op, not just an error.
//!
//! `accept_admin` reads `DataKey::PendingAdmin` and returns
//! `Error::NoPendingTransfer` before it authenticates anyone or writes the
//! admin. On success it sets `DataKey::Admin` and removes the pending key.
//!
//! `src/test.rs` covers both shapes of the failure — `test_accept_admin_fails_without_pending_transfer`
//! calls cold, and `test_double_accept_admin_fails` calls again after a
//! completed transfer — but neither re-reads state afterwards, so each one
//! asserts only the returned code. A `accept_admin` that wrote `Admin` first
//! and checked the pending entry second would satisfy both, and so would one
//! that cleared a pending proposal it never accepted. The cold path is what a
//! keeper hits after a restart, which is why the state has to come back
//! unchanged.
//!
//! These tests assert through the public getters, with `mock_all_auths` left
//! on: the point is the missing pending entry, not who signed.

use accensa_common::Error;
use receipt_anchor::{ReceiptAnchor, ReceiptAnchorClient};
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env};

mod shard_wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/receipt_shard.wasm");
}

const SHARD: u64 = 0;

fn setup() -> (Env, ReceiptAnchorClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let anchor_id = env.register(ReceiptAnchor, ());
    let anchor = ReceiptAnchorClient::new(&env, &anchor_id);
    let shard_wasm_hash = env.deployer().upload_contract_wasm(shard_wasm::WASM);
    anchor.initialize(&merchant, &shard_wasm_hash);

    (env, anchor, merchant)
}

fn root_at(env: &Env, i: u8) -> BytesN<32> {
    BytesN::from_array(env, &[i; 32])
}

/// A cold call — no transfer has ever been proposed — fails with the typed
/// error and leaves both sides of the hand-off exactly as they were.
#[test]
fn a_cold_accept_fails_and_leaves_the_admin_in_place() {
    let (_env, anchor, merchant) = setup();
    assert_eq!(anchor.get_admin(), merchant);
    assert_eq!(
        anchor.try_get_pending_admin(),
        Err(Ok(Error::NoPendingTransfer)),
        "precondition: nothing is pending"
    );

    assert_eq!(
        anchor.try_accept_admin(),
        Err(Ok(Error::NoPendingTransfer)),
        "accepting with no proposal must fail"
    );

    // Re-read both states the call could have moved.
    assert_eq!(
        anchor.get_admin(),
        merchant,
        "a rejected accept must not promote anybody"
    );
    assert_eq!(
        anchor.try_get_pending_admin(),
        Err(Ok(Error::NoPendingTransfer)),
        "and it must not invent or clear a pending proposal"
    );
}

/// The rejected cold call wrote nothing at all: the merchant keeps its
/// authority afterwards, so a `accept_admin` that failed *after* overwriting
/// `Admin` would be caught here rather than at the returned code.
#[test]
fn the_merchant_can_still_write_after_a_cold_accept_fails() {
    let (env, anchor, _merchant) = setup();

    assert_eq!(anchor.try_accept_admin(), Err(Ok(Error::NoPendingTransfer)));

    // The admin is whoever the contract reads for authorization, so an action
    // that only the original merchant can take still goes through.
    anchor.set_min_anchor_interval(&60);
    assert_eq!(anchor.get_min_anchor_interval(), 60);

    let root = root_at(&env, 1);
    assert_eq!(
        anchor.try_anchor_batch(&SHARD, &root, &1, &0, &10),
        Ok(Ok(1)),
        "anchoring is still authorized by the original admin after the \
         rejected accept"
    );
    assert_eq!(anchor.get_root_buffer(&SHARD).len(), 1);
}

/// After a transfer completes, the proposal is consumed — so the same call that
/// just succeeded now fails, and the new admin is still the admin.
#[test]
fn accepting_twice_does_not_undo_the_completed_transfer() {
    let (env, anchor, merchant) = setup();
    let proposed = Address::generate(&env);

    anchor.transfer_admin(&proposed);
    assert_eq!(anchor.try_accept_admin(), Ok(Ok(())));
    assert_eq!(anchor.get_admin(), proposed, "precondition");

    assert_eq!(
        anchor.try_accept_admin(),
        Err(Ok(Error::NoPendingTransfer)),
        "there is no second proposal to accept"
    );

    // Re-read both states after the failure.
    assert_eq!(
        anchor.get_admin(),
        proposed,
        "the failed accept must not roll the admin back to {merchant:?}"
    );
    assert_ne!(anchor.get_admin(), merchant);
    assert_eq!(
        anchor.try_get_pending_admin(),
        Err(Ok(Error::NoPendingTransfer)),
        "and it must not resurrect the proposal it consumed"
    );
}

/// Once a proposal has been consumed, a later accept fails and moves nothing:
/// the admin stays whoever the completed transfer installed, and neither the
/// former admin nor the intermediate one comes back.
#[test]
fn a_consumed_proposal_cannot_be_accepted_again() {
    let (env, anchor, merchant) = setup();
    let first = Address::generate(&env);
    let second = Address::generate(&env);

    // Hand off to `first`, then to `second`.
    anchor.transfer_admin(&first);
    assert_eq!(anchor.try_accept_admin(), Ok(Ok(())));
    anchor.transfer_admin(&second);

    assert_eq!(anchor.get_admin(), first);
    assert_eq!(anchor.get_pending_admin(), second);

    assert_eq!(anchor.try_accept_admin(), Ok(Ok(())));
    assert_eq!(anchor.get_admin(), second);
    assert_eq!(
        anchor.try_get_pending_admin(),
        Err(Ok(Error::NoPendingTransfer)),
        "the proposal is spent"
    );

    // Now cold again, under the new admin.
    assert_eq!(anchor.try_accept_admin(), Err(Ok(Error::NoPendingTransfer)));
    assert_eq!(
        anchor.get_admin(),
        second,
        "the rejected accept left the current admin alone"
    );
    assert_ne!(anchor.get_admin(), merchant);
    assert_ne!(anchor.get_admin(), first);
}
