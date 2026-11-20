//! Integration tests for in-flight ragequit behavior when the treasury token rotates (issue #1024).

#![cfg(test)]

use governance::{DataKey, Error, Governance, GovernanceClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    token::{StellarAssetClient, TokenClient},
    Address, Env, Symbol, Val, Vec,
};

struct Harness {
    env: Env,
    gov_id: Address,
    gov: GovernanceClient<'static>,
    token_a: Address,
    token_b: Address,
    token_c: Address,
    m1: Address,
    m2: Address,
    m3: Address,
}

fn setup() -> Harness {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| li.sequence_number = 100);

    let m1 = Address::generate(&env);
    let m2 = Address::generate(&env);
    let m3 = Address::generate(&env);

    let members = Vec::from_array(&env, [m1.clone(), m2.clone(), m3.clone()]);
    let deposits = Vec::from_array(&env, [10u64, 10u64, 20u64]);

    let gov_id = env.register(Governance, (members, deposits, 5000u32, 100u32));
    let gov = GovernanceClient::new(&env, &gov_id);

    let sac_a = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let token_a = sac_a.address();
    let sac_b = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let token_b = sac_b.address();
    let sac_c = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let token_c = sac_c.address();

    // Fund governance with initial token A and configure as treasury
    StellarAssetClient::new(&env, &token_a).mint(&gov_id, &100_000);
    env.as_contract(&gov_id, || {
        env.storage()
            .instance()
            .set(&DataKey::TreasuryToken, &token_a);
    });

    Harness {
        env,
        gov_id,
        gov,
        token_a,
        token_b,
        token_c,
        m1,
        m2,
        m3,
    }
}

#[test]
fn test_in_flight_ragequit_uses_rotated_treasury_token() {
    let h = setup();

    // 1. Create a proposal
    let pid = h.gov.propose(
        &h.m3,
        &h.gov_id,
        &Symbol::new(&h.env, "test_call"),
        &Vec::<Val>::new(&h.env),
    );

    // 2. Member m1 dissents (votes NO)
    h.gov.vote(&h.m1, &pid, &false);

    // Members m2 and m3 vote YES, meeting threshold/quorum and approving the proposal
    h.gov.vote(&h.m2, &pid, &true);
    h.gov.vote(&h.m3, &pid, &true);

    // In-flight ragequit window is now open for dissenter m1.
    // 3. Governance rotates treasury token from token A to token B
    StellarAssetClient::new(&h.env, &h.token_b).mint(&h.gov_id, &200_000);
    h.env.as_contract(&h.gov_id, || {
        h.env
            .storage()
            .instance()
            .set(&DataKey::TreasuryToken, &h.token_b);
    });

    // 4. Dissenting member m1 invokes ragequit
    let res = h.gov.try_ragequit(&h.m1, &pid);
    assert_eq!(res, Ok(Ok(())));

    // Asserts ragequit paid out from newly active token B, not token A
    let bal_b = TokenClient::new(&h.env, &h.token_b).balance(&h.m1);
    let bal_a = TokenClient::new(&h.env, &h.token_a).balance(&h.m1);
    assert!(bal_b > 0, "m1 must receive rotated token B payout");
    assert_eq!(bal_a, 0, "token A must not be touched");

    // Member deposit and voting weight are consumed; second ragequit is rejected
    let res_repeat = h.gov.try_ragequit(&h.m1, &pid);
    assert_eq!(res_repeat, Err(Ok(Error::AlreadyRagequit)));
}

#[test]
fn test_in_flight_ragequit_with_unfunded_rotated_token_completes_gracefully() {
    let h = setup();

    let pid = h.gov.propose(
        &h.m3,
        &h.gov_id,
        &Symbol::new(&h.env, "test_call"),
        &Vec::<Val>::new(&h.env),
    );

    h.gov.vote(&h.m1, &pid, &false);
    h.gov.vote(&h.m2, &pid, &true);
    h.gov.vote(&h.m3, &pid, &true);

    // Rotate treasury token to token C, which has 0 balance in the contract
    h.env.as_contract(&h.gov_id, || {
        h.env
            .storage()
            .instance()
            .set(&DataKey::TreasuryToken, &h.token_c);
    });

    // Ragequit against unfunded rotated token completes gracefully with 0 payout
    let res = h.gov.try_ragequit(&h.m1, &pid);
    assert_eq!(res, Ok(Ok(())));

    let bal_c = TokenClient::new(&h.env, &h.token_c).balance(&h.m1);
    assert_eq!(bal_c, 0);
}
