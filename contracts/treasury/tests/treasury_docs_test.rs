//! Integration tests verifying Treasury public getter parity with documentation (issue #1015).

#![cfg(test)]

use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Address, Env,
};
use treasury::{Treasury, TreasuryClient};

#[allow(dead_code)]
struct Fixture {
    env: Env,
    client: TreasuryClient<'static>,
    admin: Address,
    token: Address,
    beneficiary: Address,
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| li.timestamp = 1_700_000_000);

    let admin = Address::generate(&env);
    let beneficiary = Address::generate(&env);

    let sac = env.register_stellar_asset_contract_v2(admin.clone());
    let token = sac.address();

    let contract_id = env.register(Treasury, (admin.clone(), token.clone()));
    let client = TreasuryClient::new(&env, &contract_id);

    StellarAssetClient::new(&env, &token).mint(&contract_id, &10_000_000);

    Fixture {
        env,
        client,
        admin,
        token,
        beneficiary,
    }
}

#[test]
fn test_treasury_public_getters_match_docs() {
    let fx = setup();

    // 1. Core Config Getters
    assert_eq!(fx.client.get_admin(), Some(fx.admin.clone()));
    assert_eq!(fx.client.get_token(), Some(fx.token.clone()));
    assert_eq!(fx.client.get_reserve_bps(), 10_000);
    assert_eq!(fx.client.get_deployed_total(), 0);
    assert_eq!(fx.client.get_yield_earned(), 0);

    // 2. Unconfigured options return None or empty
    assert_eq!(fx.client.get_amm(), None);
    assert_eq!(fx.client.get_stable_token(), None);
    assert_eq!(fx.client.get_price_feed(), None);
    assert_eq!(fx.client.get_buyback_config(), None);
    assert_eq!(fx.client.get_allocations().len(), 0);

    // 3. Vesting schedule getters
    assert_eq!(fx.client.get_schedule(&fx.beneficiary), None);

    fx.client
        .add_schedule(&fx.beneficiary, &1_000_000, &1_700_000_000, &1_000, &4_000);

    let sched = fx.client.get_schedule(&fx.beneficiary).unwrap();
    assert_eq!(sched.total, 1_000_000);
    assert_eq!(sched.claimed, 0);
    assert_eq!(sched.start, 1_700_000_000);
    assert_eq!(sched.cliff, 1_000);
    assert_eq!(sched.duration, 4_000);
}
