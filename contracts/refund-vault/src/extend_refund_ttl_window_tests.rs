//! Issue #686: `extend_refund_ttl` answers a missing `DataKey::RefundWindow`
//! with a typed error instead of a host trap.
//!
//! The top-up sizes the extension from the refund window held in instance
//! storage. That read used to be an `unwrap()`, so a call that found a refund
//! record but no window aborted with an untyped `WasmRuntimeError` instead of
//! returning a variant of `Error` like the missing-record arm does.
//!
//! The test stores a v2 refund record directly, removes the window, and asserts
//! the returned variant.

use soroban_sdk::{testutils::Address as _, Address, BytesN, Env};

use crate::test_helpers::vault_init;
use crate::{DataKey, Error, RefundRecord, RefundVault, RefundVaultClient};

#[test]
fn extend_refund_ttl_without_a_stored_window_returns_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();

    let vault_id = env.register(RefundVault, (vault_init(&env, &merchant, &token, 17_280),));
    let vault = RefundVaultClient::new(&env, &vault_id);

    let payment_ref = BytesN::from_array(&env, &[8u8; 32]);
    let buyer = Address::generate(&env);

    env.as_contract(&vault_id, || {
        let ledger = env.ledger().sequence();
        env.storage().persistent().set(
            &DataKey::RefundV2(payment_ref.clone()),
            &RefundRecord {
                amount_refunded: 40_000,
                payment_amount: 100_000,
                paid_at_ledger: ledger,
                recipient: buyer.clone(),
                ledger,
            },
        );
        // The record exists, so the call gets past `RefundNotFound` and reaches
        // the window read.
        env.storage().instance().remove(&DataKey::RefundWindow);
    });

    assert_eq!(
        vault.try_extend_refund_ttl(&payment_ref),
        Err(Ok(Error::NotInitialized))
    );
}
