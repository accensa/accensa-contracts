//! Boundary test for `claim_single`'s checked cumulative sum (issue #718).
//!
//! `claim_single` computed `previous_refunded + claim.amount` with a bare `+`.
//! With `[profile.release]` enabling `overflow-checks` and `panic = "abort"`,
//! an unrepresentable cumulative total aborted the host call instead of
//! answering with a typed error. The sum now routes through
//! `accensa_common::math::add_amounts`, whose `MathError::Overflow` maps to
//! `Error::MathOverflow`.
//!
//! `settlement::resolve_ceiling` checks the same sum earlier in the claim
//! path, so this boundary is expected to answer with the typed variant from
//! there; the guard in `claim_single` keeps its own write honest and is
//! exercised whenever the two checks are read separately. The test pins the
//! stored cumulative total at `i128::MAX - 1` and claims two more units.

use soroban_sdk::{testutils::Address as _, Address, BytesN, Env};

use crate::test_helpers::vault_init;
use crate::{DataKey, Error, RefundRecord, RefundVault, RefundVaultClient};

fn setup() -> (Env, RefundVaultClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();

    let vault_id = env.register(RefundVault, (vault_init(&env, &merchant, &token, 17_280),));
    let vault = RefundVaultClient::new(&env, &vault_id);

    (env, vault, vault_id)
}

#[test]
fn overflowing_cumulative_refund_returns_math_overflow() {
    let (env, vault, vault_id) = setup();
    let recipient = Address::generate(&env);
    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let paid_at = env.ledger().sequence();

    // A payment whose recorded cumulative refund is one unit short of the
    // type's maximum, against a ceiling that still admits it.
    env.as_contract(&vault_id, || {
        env.storage().persistent().set(
            &DataKey::RefundV2(payment_ref.clone()),
            &RefundRecord {
                amount_refunded: i128::MAX - 1,
                payment_amount: i128::MAX,
                paid_at_ledger: paid_at,
                recipient: recipient.clone(),
                ledger: paid_at,
            },
        );
    });

    assert_eq!(
        vault.try_refund(
            &payment_ref,
            &recipient,
            &2,
            &paid_at,
            &i128::MAX,
            &None,
            &0,
        ),
        Err(Ok(Error::MathOverflow))
    );
}
