//! Boundary test for `preview_settlement`'s checked cumulative sum
//! (issue #722).
//!
//! `preview_settlement` computed `previous_refunded + amount` with a bare
//! `+`. With `[profile.release]` setting `overflow-checks = true` and
//! `panic = "abort"`, a non-representable sum would abort the host call with a
//! `WasmRuntimeError` instead of answering with a typed error. The sum now
//! routes through `accensa_common::math::add_amounts`, whose failure maps to
//! `Error::MathOverflow`.
//!
//! The tests below pin **both** operands of that sum at `i128::MAX`. The
//! ceiling gate in `resolve_ceiling` rejects a non-representable cumulative
//! total before the sum is computed (`Error::ExceedsPayment`), so every
//! overflowing input resolves to a typed variant — never a panic — and the
//! `add_amounts` guard behind it is the defensive backstop.

use soroban_sdk::{testutils::Address as _, Address, BytesN, Env};

use crate::test_helpers::vault_init;
use crate::{DataKey, Error, RefundRecord, RefundVault, RefundVaultClient};

fn setup() -> (Env, Address, RefundVaultClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(merchant.clone())
        .address();

    let vault_id = env.register(RefundVault, (vault_init(&env, &merchant, &token, 0),));
    let client = RefundVaultClient::new(&env, &vault_id);
    (env, vault_id, client)
}

/// Write a cumulative-refund record straight into the vault's persistent
/// storage so a boundary total no live refund could ever produce is
/// observable from `preview_settlement`.
fn seed_record(env: &Env, vault: &Address, pref: &BytesN<32>, refunded: i128, ceiling: i128) {
    env.as_contract(vault, || {
        env.storage().persistent().set(
            &DataKey::RefundV2(pref.clone()),
            &RefundRecord {
                amount_refunded: refunded,
                payment_amount: ceiling,
                paid_at_ledger: 0,
                recipient: Address::generate(env),
                ledger: 0,
            },
        );
    });
}

#[test]
fn an_unrepresentable_cumulative_sum_is_a_typed_error_not_a_host_abort() {
    let (env, vault, client) = setup();

    // First operand pinned at the ceiling: `i128::MAX` already refunded, a
    // further 1 makes the sum at settlement.rs:139 unrepresentable.
    let pref_max_previous = BytesN::from_array(&env, &[1u8; 32]);
    seed_record(&env, &vault, &pref_max_previous, i128::MAX, i128::MAX);
    assert_eq!(
        client.try_preview_settlement(&pref_max_previous, &1, &i128::MAX),
        Err(Ok(Error::ExceedsPayment))
    );

    // Second operand pinned at the ceiling: an amount of `i128::MAX` added to
    // a payment that already has 1 refunded. Same unrepresentable sum, same
    // typed answer — no host abort either way.
    let pref_max_amount = BytesN::from_array(&env, &[2u8; 32]);
    seed_record(&env, &vault, &pref_max_amount, 1, i128::MAX);
    assert_eq!(
        client.try_preview_settlement(&pref_max_amount, &i128::MAX, &i128::MAX),
        Err(Ok(Error::ExceedsPayment))
    );
}
