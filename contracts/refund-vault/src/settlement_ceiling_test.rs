#![cfg(test)]

//! Boundary test for the checked ceiling arithmetic (issue #721).
//!
//! `resolve_ceiling` routes the cumulative sum through
//! `accensa_common::math::add_amounts`, so a refund that pushes the
//! cumulative total past `i128::MAX` must surface as the typed
//! [`accensa_common::Error::MathOverflow`] — read-only callers get a typed
//! error they can report, not a host abort from a bare `+` under
//! `overflow-checks`.

use soroban_sdk::{
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env,
};

use crate::test_helpers::vault_init;
use crate::{RefundVault, RefundVaultClient};

fn setup(float: i128) -> (Env, RefundVaultClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(merchant.clone())
        .address();

    let vault = env.register(RefundVault, (vault_init(&env, &merchant, &token, 0),));
    let client = RefundVaultClient::new(&env, &vault);

    StellarAssetClient::new(&env, &token).mint(&merchant, &float);
    client.deposit(&merchant, &float, &None);

    (env, client, token)
}

#[test]
fn a_cumulative_refund_past_i128_max_reports_math_overflow() {
    let (env, client, token) = setup(i128::MAX);
    let extreme = i128::MAX;

    // First claim refunds the whole `i128` range, parking `amount_refunded` at
    // the type's maximum in the stored refund record. Window `0` is "no time
    // bound" (see `refund`), so no ledger shaping is needed.
    let pref = BytesN::from_array(&env, &[21u8; 32]);
    let buyer = Address::generate(&env);
    client.refund(&pref, &buyer, &extreme, &0, &extreme, &None, &0);
    assert_eq!(
        TokenClient::new(&env, &token).balance(&buyer),
        extreme,
        "the boundary refund must have fully applied"
    );

    // A further partial against the same payment would push the cumulative sum
    // one unit past `i128::MAX`: the overflow arm must be the typed failure.
    assert_eq!(
        client.try_preview_settlement(&pref, &1, &extreme),
        Err(Ok(accensa_common::Error::MathOverflow))
    );
}
