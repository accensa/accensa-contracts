//! #955 — the admin hand-off window is authorization-relevant, and nothing on
//! `main` exercises it.
//!
//! `transfer_admin` writes only `DataKey::PendingAdmin`; `accept_admin` is what
//! moves `DataKey::Admin`, and it authenticates as the *proposed* address.
//! `anchor_batch` authorizes against `DataKey::Admin`, so during the window
//! between the two calls the old admin can still write roots and the proposed
//! one cannot.
//!
//! `src/test.rs` checks the getters, and `tests/integration_test.rs` walks the
//! flow under `mock_all_auths` — which authorizes whatever address the contract
//! asks for, so it proves nothing about who is *allowed*. The one scoped-auth
//! test there runs after `accept_admin`. A `transfer_admin` that wrote `Admin`
//! directly, or an anchor path that accepted the pending address early, would
//! pass everything on `main`. Each call below mocks auth for one named
//! address, so both of those changes fail here.

use accensa_common::Error;
use receipt_anchor::{ReceiptAnchor, ReceiptAnchorClient};
use soroban_sdk::{
    testutils::{Address as _, MockAuth, MockAuthInvoke},
    vec, Address, BytesN, Env, IntoVal, Val, Vec,
};

/// Logical shard used by these tests.
const DEFAULT_SHARD: u64 = 0;

mod shard_wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/receipt_shard.wasm");
}

/// Initialize as `admin`, then propose `proposed` — the pending window.
///
/// `mock_all_auths` is used only for setup; every assertion switches to scoped
/// auth so the host actually evaluates who signed.
fn pending_window() -> (Env, ReceiptAnchorClient<'static>, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let proposed = Address::generate(&env);

    let anchor_id = env.register(ReceiptAnchor, ());
    let anchor = ReceiptAnchorClient::new(&env, &anchor_id);
    anchor.initialize(
        &admin,
        &env.deployer().upload_contract_wasm(shard_wasm::WASM),
    );
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    anchor.set_bounty_asset(&token);

    // The proposal is itself an admin action.
    anchor.transfer_admin(&proposed);

    (env, anchor, admin, proposed)
}

/// Authorize exactly `address` for one `anchor_batch` of `root`, and return the
/// contract's outcome. The mocked argument vector is the one the host replays,
/// so it is built from the same literals as the call below.
/// Authorize exactly `address` for the next `anchor_batch` of `root`. The
/// mocked argument vector is the one the host replays, so it is built from the
/// same literals as the call the tests below make.
fn mock_anchor_auth(env: &Env, anchor: &ReceiptAnchorClient, address: &Address, root: &BytesN<32>) {
    let args: Vec<Val> = vec![
        env,
        DEFAULT_SHARD.into_val(env),
        root.clone().into_val(env),
        1u32.into_val(env),
        0u64.into_val(env),
        10u64.into_val(env),
    ];
    env.mock_auths(&[MockAuth {
        address,
        invoke: &MockAuthInvoke {
            contract: &anchor.address,
            fn_name: "anchor_batch",
            args: args.clone(),
            sub_invokes: &[],
        },
    }]);
}

/// The anchored shape used by every call in this file: shard 0, count 1,
/// period 0..10, so the mocked argument vector and the real call cannot drift.
fn root_at(env: &Env, i: u8) -> BytesN<32> {
    BytesN::from_array(env, &[i; 32])
}

/// The old admin's authority does not lapse when it proposes a successor: the
/// pending entry is a proposal, not a hand-off.
#[test]
fn old_admin_can_still_anchor_while_the_transfer_is_pending() {
    let (env, anchor, admin, proposed) = pending_window();

    assert_eq!(anchor.get_admin(), admin, "the proposal moved nothing");
    assert_eq!(anchor.get_pending_admin(), proposed);

    let root = root_at(&env, 1);
    mock_anchor_auth(&env, &anchor, &admin, &root);
    assert_eq!(
        anchor.try_anchor_batch(&DEFAULT_SHARD, &root, &1, &0, &10),
        Ok(Ok(1)),
        "the current admin must still be able to anchor during the window"
    );
    assert_eq!(
        anchor.get_admin(),
        admin,
        "anchoring must not consume the pending proposal"
    );
    assert_eq!(anchor.get_pending_admin(), proposed);
}

/// The proposed address has no authority yet: an authorization that is valid in
/// shape but signed by the wrong principal is refused, and it is refused by the
/// host rather than by a contract error — the outer `Err` here is
/// `soroban_sdk::Error`, while a business-rule rejection would arrive as
/// `Err(Ok(Error::..))`.
#[test]
fn proposed_admin_cannot_anchor_before_accepting() {
    let (env, anchor, admin, proposed) = pending_window();

    let root = root_at(&env, 2);
    mock_anchor_auth(&env, &anchor, &proposed, &root);
    let rejected = anchor.try_anchor_batch(&DEFAULT_SHARD, &root, &1, &0, &10);

    assert!(
        rejected.is_err(),
        "the pending admin must not be able to write roots before accepting"
    );
    assert!(
        !matches!(rejected, Err(Ok(_))),
        "the refusal must be the host's authorization failure, not a contract \
         error a caller could read as a business rule: {rejected:?}"
    );

    // Nothing was written by the refused call.
    assert_eq!(anchor.get_batch_count(&DEFAULT_SHARD), 0);
    assert_eq!(anchor.get_admin(), admin);
    assert_eq!(anchor.get_pending_admin(), proposed);
}

/// After `accept_admin` the authority has moved: the former admin is refused
/// with the same host-level failure, the new admin is accepted, and the getters
/// agree with the enforcement.
#[test]
fn roles_swap_only_after_accept_admin() {
    let (env, anchor, admin, proposed) = pending_window();

    // Accepting authenticates as the proposed address, and takes no arguments.
    env.mock_auths(&[MockAuth {
        address: &proposed,
        invoke: &MockAuthInvoke {
            contract: &anchor.address,
            fn_name: "accept_admin",
            args: Vec::new(&env),
            sub_invokes: &[],
        },
    }]);
    assert_eq!(anchor.try_accept_admin(), Ok(Ok(())));

    assert_eq!(anchor.get_admin(), proposed, "the hand-off is effective");

    let root = root_at(&env, 3);
    mock_anchor_auth(&env, &anchor, &admin, &root);
    let former = anchor.try_anchor_batch(&DEFAULT_SHARD, &root, &1, &0, &10);
    assert!(
        !matches!(former, Err(Ok(_))) && former.is_err(),
        "the former admin must lose the ability to anchor: {former:?}"
    );

    mock_anchor_auth(&env, &anchor, &proposed, &root);
    assert_eq!(
        anchor.try_anchor_batch(&DEFAULT_SHARD, &root, &1, &0, &10),
        Ok(Ok(1)),
        "the accepted admin must be able to anchor"
    );
}

/// Completing the transfer clears the proposal, so there is no residual
/// authority to hand to a later caller.
#[test]
fn pending_entry_is_consumed_by_the_acceptance() {
    let (_env, anchor, _admin, proposed) = pending_window();
    anchor.accept_admin();

    assert_eq!(anchor.get_admin(), proposed);
    assert_eq!(
        anchor.try_get_pending_admin(),
        Err(Ok(Error::NoPendingTransfer)),
        "a completed transfer leaves no pending admin behind"
    );
}
