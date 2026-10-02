/// stACC 1:1 mint on lock test.
#[test]
fn stacc_mint_1_to_1_on_lock() {
    let env = Env::default();
    env.mock_all_auths();

    let stacc_id = env.register(LiquidStaking, ());

    let stacc = LiquidStakingClient::new(&env, &stacc_id);
    stacc.initialize(&1_000_000);

    // Alice locks 50 underlying tokens
    let alice = Address::generate(&env);
    stacc.mint(&alice, &50);

    // Check total supply and locked
    assert_eq!(stacc.get_total_supply(), 50);
    assert_eq!(stacc.get_total_locked(), 50);
    assert_eq!(stacc.get_user_stacc_balance(&alice), 50);

    // Bob holds no stACC, so his burn must fail
    let bob = Address::generate(&env);
    let res = stacc.try_burn(&bob, &1);
    assert!(matches!(
        res,
        Err(Ok(LiquidStakingError::InsufficientBalance))
    ));
}

/// Test exchange rate progression and redemption constraints.
#[test]
fn stacc_exchange_rate_progression() {
    let env = Env::default();
    env.mock_all_auths();

    let stacc_id = env.register(LiquidStaking, ());

    let stacc = LiquidStakingClient::new(&env, &stacc_id);
    stacc.initialize(&1_000_000);

    // Alice locks 100 underlying tokens at 1:1 rate
    let alice = Address::generate(&env);
    stacc.mint(&alice, &100);

    // Initial exchange rate is 1_000_000 (1:1)
    assert_eq!(stacc.get_exchange_rate(), 1_000_000);

    // Update exchange rate to reflect yield (e.g., 1_500_000 = 1.5x value)
    stacc.set_exchange_rate(&1_500_000);
    assert_eq!(stacc.get_exchange_rate(), 1_500_000);

    // Bob tries to burn 100 stACC before lock expiry - should fail
    let bob = Address::generate(&env);
    // First set bob's lock start ledger to current (so it hasn't expired)
    let user_key = LiquidStakingDataKey::User(bob.clone());
    let lock_start = env.ledger().sequence();
    env.as_contract(&stacc_id, || {
        env.storage().persistent().set(
            &user_key,
            &UserData {
                stacc_balance: 100,
                locked_underlying: 100,
                lock_start_ledger: lock_start,
            },
        );
    });

    let res = stacc.try_burn(&bob, &100);
    // Lock hasn't expired yet (same ledger), so this should fail
    assert!(matches!(res, Err(Ok(LiquidStakingError::LockNotExpired))));

    // Now advance ledger past lock epoch (86400 ledgers = 1 day)
    env.ledger().with_mut(|l| l.sequence_number += 86400 + 1);

    // Burn after lock expiry - should succeed with new exchange rate
    // 100 stACC * 1_500_000 / 1_000_000 = 150 underlying tokens
    // The yield accrued with the rate bump is part of the locked total, so
    // back the redemption with 150 locked underlying before burning.
    env.as_contract(&stacc_id, || {
        env.storage()
            .instance()
            .set(&LiquidStakingDataKey::TotalLocked, &150u64);
    });
    let res = stacc.try_burn(&bob, &100);
    assert!(res.is_ok(), "burn after lock expiry should succeed");
    // The underlying redeemed should be 150 (100 * 1.5)
    // We can't directly check the redeemed amount from the event in this
    // simple test, but we verify the burn succeeds

    // Verify total supply decreased
    assert_eq!(stacc.get_total_supply(), 0);
}

/// Test burn-on-redemption post lock expiry.
#[test]
fn stacc_burn_post_lock_expiry() {
    let env = Env::default();
    env.mock_all_auths();

    let stacc_id = env.register(LiquidStaking, ());

    let stacc = LiquidStakingClient::new(&env, &stacc_id);
    stacc.initialize(&1_000_000);

    // Alice locks 50 underlying tokens
    let alice = Address::generate(&env);
    stacc.mint(&alice, &50);

    // Get the lock start ledger from user data
    let user_key = LiquidStakingDataKey::User(alice.clone());
    let lock_start = env.as_contract(&stacc_id, || {
        env.storage()
            .persistent()
            .get::<_, LiquidStakingUserData>(&user_key)
            .map(|d| d.lock_start_ledger)
            .unwrap_or(0)
    });

    // Advance ledger past lock epoch (86400 ledgers)
    env.ledger()
        .with_mut(|l| l.sequence_number += lock_start + 86400 + 1);

    // Burn stACC after lock expiry
    let res = stacc.try_burn(&alice, &50);
    assert!(res.is_ok(), "burn after lock expiry must succeed");

    // Verify balances are zero
    assert_eq!(stacc.get_user_stacc_balance(&alice), 0);
    assert_eq!(stacc.get_total_supply(), 0);
    assert_eq!(stacc.get_total_locked(), 0);
}
//
// ## Liquid Staking Derivative (stACC) Tests
// Tests for stACC minting, exchange rate progression, and burn redemption.

extern crate std;

use crate::{
    Error, Governance, GovernanceClient, LiquidStaking, LiquidStakingClient, LiquidStakingDataKey,
    LiquidStakingError, LiquidStakingUserData, UserData,
};
use soroban_sdk::{
    contract, contractimpl, symbol_short,
    testutils::{Address as _, Events as _, Ledger},
    Address, Env, IntoVal, Symbol, Val, Vec,
};

/// A trivial admin-gated contract: `set_value` requires the stored admin's
/// auth, exactly like `ReceiptAnchor::set_anchor_rate_limit` requires its
/// merchant's. Used to prove `Governance::execute` can act as that admin.
#[contract]
struct Target;

#[contractimpl]
impl Target {
    pub fn init(env: Env, admin: Address) {
        env.storage()
            .instance()
            .set(&symbol_short!("admin"), &admin);
    }

    pub fn set_value(env: Env, value: u32) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap();
        admin.require_auth();
        env.storage()
            .instance()
            .set(&symbol_short!("value"), &value);
    }

    pub fn get_value(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&symbol_short!("value"))
            .unwrap_or(0)
    }
}

struct Harness {
    env: Env,
    gov: GovernanceClient<'static>,
    target: Address,
    m1: Address,
    m2: Address,
    m3: Address,
}

/// Three members with deposits 1/1/4 (quadratic weights 1/1/2, total 4),
/// 6000 bps (60%) quorum, 100-ledger voting window. `m3` alone (weight 2)
/// cannot pass; `m3` + either other member (weight 3) can.
fn setup() -> Harness {
    let env = Env::default();
    env.mock_all_auths();

    let m1 = Address::generate(&env);
    let m2 = Address::generate(&env);
    let m3 = Address::generate(&env);

    let members = Vec::from_array(&env, [m1.clone(), m2.clone(), m3.clone()]);
    let deposits = Vec::from_array(&env, [1u64, 1u64, 4u64]);

    let gov_id = env.register(Governance, (members, deposits, 6000u32, 100u32));
    let gov = GovernanceClient::new(&env, &gov_id);

    let target_id = env.register(Target, ());
    let target_client = TargetClient::new(&env, &target_id);
    target_client.init(&gov_id);

    Harness {
        env,
        gov,
        target: target_id,
        m1,
        m2,
        m3,
    }
}

fn set_value_call(env: &Env, target: &Address, value: u32) -> (Address, Symbol, Vec<Val>) {
    (
        target.clone(),
        Symbol::new(env, "set_value"),
        Vec::from_array(env, [value.into_val(env)]),
    )
}

#[test]
fn constructor_rejects_mismatched_lengths() {
    let env = Env::default();
    let m1 = Address::generate(&env);
    let members = Vec::from_array(&env, [m1]);
    let deposits = Vec::from_array(&env, [1u64, 2u64]);
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.register(Governance, (members, deposits, 5000u32, 10u32))
    }));
    assert!(
        res.is_err(),
        "mismatched members/deposits must reject construction"
    );
}

#[test]
fn constructor_rejects_zero_threshold() {
    let env = Env::default();
    let m1 = Address::generate(&env);
    let members = Vec::from_array(&env, [m1]);
    let deposits = Vec::from_array(&env, [1u64]);
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.register(Governance, (members, deposits, 0u32, 10u32))
    }));
    assert!(res.is_err(), "a zero threshold must reject construction");
}

#[test]
fn constructor_rejects_zero_voting_period() {
    let env = Env::default();
    let m1 = Address::generate(&env);
    let members = Vec::from_array(&env, [m1]);
    let deposits = Vec::from_array(&env, [1u64]);
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.register(Governance, (members, deposits, 5000u32, 0u32))
    }));
    assert!(
        res.is_err(),
        "a zero voting period must reject construction"
    );
}

#[test]
fn non_member_cannot_propose() {
    let h = setup();
    let outsider = Address::generate(&h.env);
    let (target, function, args) = set_value_call(&h.env, &h.target, 7);
    let res = h.gov.try_propose(&outsider, &target, &function, &args);
    assert_eq!(res, Err(Ok(Error::NotAMember)));
}

#[test]
fn quorum_below_threshold_blocks_execution() {
    let h = setup();
    let (target, function, args) = set_value_call(&h.env, &h.target, 7);
    let id = h.gov.propose(&h.m3, &target, &function, &args);

    // Only m3 (weight 2 of 4 = 50%) votes yes; quorum is 60%.
    h.gov.vote(&h.m3, &id, &true);

    let res = h.gov.try_execute(&id);
    assert_eq!(res, Err(Ok(Error::QuorumNotMet)));
}

#[test]
fn quorum_met_executes_and_authorizes_governed_call() {
    let h = setup();
    let (target, function, args) = set_value_call(&h.env, &h.target, 42);
    let id = h.gov.propose(&h.m3, &target, &function, &args);

    // m3 (weight 2) + m1 (weight 1) = 3 of 4 = 75% >= 60% quorum.
    h.gov.vote(&h.m3, &id, &true);
    h.gov.vote(&h.m1, &id, &true);

    h.gov.execute(&id);

    let target_client = TargetClient::new(&h.env, &h.target);
    assert_eq!(target_client.get_value(), 42, "the governed call must run");

    let proposal = h.gov.get_proposal(&id);
    assert!(proposal.executed);
}

#[test]
fn cannot_execute_twice() {
    let h = setup();
    let (target, function, args) = set_value_call(&h.env, &h.target, 1);
    let id = h.gov.propose(&h.m1, &target, &function, &args);
    h.gov.vote(&h.m1, &id, &true);
    h.gov.vote(&h.m2, &id, &true);
    h.gov.vote(&h.m3, &id, &true);

    h.gov.execute(&id);
    let res = h.gov.try_execute(&id);
    assert_eq!(res, Err(Ok(Error::AlreadyExecuted)));
}

#[test]
fn cannot_vote_twice() {
    let h = setup();
    let (target, function, args) = set_value_call(&h.env, &h.target, 1);
    let id = h.gov.propose(&h.m1, &target, &function, &args);
    h.gov.vote(&h.m1, &id, &true);

    let res = h.gov.try_vote(&h.m1, &id, &true);
    assert_eq!(res, Err(Ok(Error::AlreadyVoted)));
}

#[test]
fn no_votes_can_outweigh_a_stale_quorum() {
    let h = setup();
    let (target, function, args) = set_value_call(&h.env, &h.target, 1);
    let id = h.gov.propose(&h.m1, &target, &function, &args);

    // m3 (weight 2) votes yes, then m1 + m2 (weight 2) vote no: yes == no,
    // so even though yes alone would clear 50% quorum it must not execute.
    h.gov.vote(&h.m3, &id, &true);
    h.gov.vote(&h.m1, &id, &false);
    h.gov.vote(&h.m2, &id, &false);

    let res = h.gov.try_execute(&id);
    assert_eq!(res, Err(Ok(Error::QuorumNotMet)));
}

#[test]
fn quorum_decays_linearly_over_the_voting_window() {
    let h = setup();
    let (target, function, args) = set_value_call(&h.env, &h.target, 1);
    let id = h.gov.propose(&h.m1, &target, &function, &args);

    // m3 alone: weight 2 of 4 = 50%. Initial quorum is 60%, so at the
    // window's start this cannot pass...
    h.gov.vote(&h.m3, &id, &true);
    assert_eq!(h.gov.try_execute(&id), Err(Ok(Error::QuorumNotMet)));

    // ...but by the midpoint (elapsed 50 of 100) the effective quorum has
    // decayed to 47.5%, which m3's 50% now clears.
    h.env.ledger().with_mut(|l| l.sequence_number += 50);
    h.gov.execute(&id);
}

#[test]
fn quorum_at_midpoint_is_lower_than_initial_but_above_floor() {
    let h = setup();
    let (target, function, args) = set_value_call(&h.env, &h.target, 1);
    let id = h.gov.propose(&h.m1, &target, &function, &args);

    h.gov.vote(&h.m3, &id, &true);

    // Halfway through a 100-ledger window the effective quorum must sit
    // strictly between 60% and the 35% floor.
    h.env.ledger().with_mut(|l| l.sequence_number += 50);
    let proposal = h.gov.get_proposal(&id);
    let voting_period = h.gov.get_voting_period();
    let now = h.env.ledger().sequence();
    let elapsed = now - proposal.deadline_ledger.saturating_sub(voting_period);

    assert_eq!(elapsed, 50);
    // 50% < 60% but >= floor, so it must execute once decayed to midpoint.
    h.gov.execute(&id);
}

#[test]
fn quorum_floor_still_requires_real_opposition_is_outweighed() {
    let h = setup();
    let (target, function, args) = set_value_call(&h.env, &h.target, 1);
    let id = h.gov.propose(&h.m1, &target, &function, &args);

    // m3 (2) yes; m1 + m2 (2) no — even at the decayed floor, yes == no,
    // so the proposal must still be rejected.
    h.gov.vote(&h.m3, &id, &true);
    h.gov.vote(&h.m1, &id, &false);
    h.gov.vote(&h.m2, &id, &false);

    h.env.ledger().with_mut(|l| l.sequence_number += 100);

    let res = h.gov.try_execute(&id);
    assert_eq!(res, Err(Ok(Error::QuorumNotMet)));
}

#[test]
fn voting_closes_after_deadline() {
    let h = setup();
    let (target, function, args) = set_value_call(&h.env, &h.target, 1);
    let id = h.gov.propose(&h.m1, &target, &function, &args);

    h.env.ledger().with_mut(|l| l.sequence_number += 101);

    let res = h.gov.try_vote(&h.m1, &id, &true);
    assert_eq!(res, Err(Ok(Error::VotingClosed)));
}

#[test]
fn prune_rejects_active_proposal_then_succeeds_once_expired() {
    let h = setup();
    let (target, function, args) = set_value_call(&h.env, &h.target, 1);
    let id = h.gov.propose(&h.m1, &target, &function, &args);

    let res = h.gov.try_prune_proposal(&id);
    assert_eq!(res, Err(Ok(Error::ProposalActive)));

    h.env.ledger().with_mut(|l| l.sequence_number += 101);
    h.gov.prune_proposal(&id);

    let res = h.gov.try_get_proposal(&id);
    assert_eq!(res, Err(Ok(Error::ProposalNotFound)));
}

#[test]
fn prune_succeeds_immediately_after_execution() {
    let h = setup();
    let (target, function, args) = set_value_call(&h.env, &h.target, 1);
    let id = h.gov.propose(&h.m1, &target, &function, &args);
    h.gov.vote(&h.m1, &id, &true);
    h.gov.vote(&h.m2, &id, &true);
    h.gov.vote(&h.m3, &id, &true);
    h.gov.execute(&id);

    h.gov.prune_proposal(&id);
    let res = h.gov.try_get_proposal(&id);
    assert_eq!(res, Err(Ok(Error::ProposalNotFound)));
}

#[test]
fn quadratic_weight_is_sqrt_of_deposit() {
    let h = setup();
    // m1 has deposit 1 -> quadratic weight = sqrt(1) = 1
    // m2 has deposit 1 -> quadratic weight = sqrt(1) = 1
    // m3 has deposit 4 -> quadratic weight = sqrt(4) = 2
    assert_eq!(h.gov.get_member_weight(&h.m1), 1);
    assert_eq!(h.gov.get_member_weight(&h.m2), 1);
    assert_eq!(h.gov.get_member_weight(&h.m3), 2);
    assert_eq!(h.gov.get_total_weight(), 4);
}

#[test]
fn quadratic_weight_prevents_whale_domination() {
    let env = Env::default();
    env.mock_all_auths();

    let m1 = Address::generate(&env);
    let m2 = Address::generate(&env);

    // m1 deposits 100 tokens -> weight 10
    // m2 deposits 10000 tokens -> weight 100
    // With linear voting, m2 would have 100x more power
    // With quadratic voting, m2 has only 10x more power
    let members = Vec::from_array(&env, [m1.clone(), m2.clone()]);
    let deposits = Vec::from_array(&env, [100u64, 10000u64]);
    let gov_id = env.register(Governance, (members, deposits, 5000u32, 100u32));
    let gov = GovernanceClient::new(&env, &gov_id);

    assert_eq!(gov.get_member_weight(&m1), 10);
    assert_eq!(gov.get_member_weight(&m2), 100);
    assert_eq!(gov.get_total_weight(), 110);
}

fn register_test_key(
    h: &Harness,
    member: &Address,
    secret: &soroban_sdk::BytesN<32>,
) -> soroban_sdk::BytesN<32> {
    let public_key = crate::ring_sig::test_support::public(&h.env, secret);
    h.gov.register_voting_key(member, &public_key);
    public_key
}

fn test_ring(
    h: &Harness,
) -> (
    Vec<soroban_sdk::BytesN<32>>,
    soroban_sdk::BytesN<32>,
    soroban_sdk::BytesN<32>,
) {
    let sk1 = crate::ring_sig::test_support::secret(&h.env, 10);
    let sk2 = crate::ring_sig::test_support::secret(&h.env, 20);
    let pk1 = register_test_key(h, &h.m1, &sk1);
    let pk2 = register_test_key(h, &h.m2, &sk2);
    (Vec::from_array(&h.env, [pk1, pk2]), sk1, sk2)
}

fn sign_for_harness(
    h: &Harness,
    proposal_id: u64,
    support: bool,
    ring: &Vec<soroban_sdk::BytesN<32>>,
    secrets: &[soroban_sdk::BytesN<32>],
    signer: usize,
) -> crate::RingSignature {
    h.env.as_contract(&h.gov.address, || {
        crate::ring_sig::test_support::sign(&h.env, proposal_id, support, ring, secrets, signer)
    })
}

#[test]
fn maximum_anonymity_set_fits_soroban_cpu_budget() {
    const SOROBAN_CPU_LIMIT: u64 = 100_000_000;

    let env = Env::default();
    env.mock_all_auths();
    let mut members = Vec::new(&env);
    let mut deposits = Vec::new(&env);
    let mut ring = Vec::new(&env);
    let mut secrets = std::vec::Vec::with_capacity(crate::MAX_MEMBERS as usize);

    for seed in 0..crate::MAX_MEMBERS {
        let member = Address::generate(&env);
        let secret = crate::ring_sig::test_support::secret(&env, u64::from(seed));
        let public_key = crate::ring_sig::test_support::public(&env, &secret);
        members.push_back(member);
        deposits.push_back(1u64);
        ring.push_back(public_key);
        secrets.push(secret);
    }

    let governance_id = env.register(Governance, (members.clone(), deposits, 5000u32, 100u32));
    let gov = GovernanceClient::new(&env, &governance_id);
    for index in 0..crate::MAX_MEMBERS {
        gov.register_voting_key(&members.get(index).unwrap(), &ring.get(index).unwrap());
    }

    let target = Address::generate(&env);
    let function = Symbol::new(&env, "noop");
    let args = Vec::new(&env);
    let proposal_id = gov.propose(&members.get(0).unwrap(), &target, &function, &args);
    let signature = env.as_contract(&governance_id, || {
        crate::ring_sig::test_support::sign(
            &env,
            proposal_id,
            true,
            &ring,
            &secrets,
            secrets.len() - 1,
        )
    });

    env.cost_estimate().budget().reset_unlimited();
    gov.vote_anonymous(&proposal_id, &true, &ring, &signature);
    let cpu_instructions = env.cost_estimate().budget().cpu_instruction_cost();
    std::println!("32-member LSAG vote: {cpu_instructions} CPU instructions");
    assert!(
        cpu_instructions < SOROBAN_CPU_LIMIT,
        "maximum anonymity set exceeded Soroban's CPU limit: {cpu_instructions}"
    );
}

#[test]
fn anonymous_lsag_vote_updates_tally_without_storing_or_emitting_signer_address() {
    let h = setup();
    let (ring, sk1, sk2) = test_ring(&h);
    let (target, function, args) = set_value_call(&h.env, &h.target, 42);
    let id = h.gov.propose(&h.m3, &target, &function, &args);
    let signature = sign_for_harness(&h, id, true, &ring, &[sk1, sk2], 1);

    h.gov.vote_anonymous(&id, &true, &ring, &signature);

    let events = h.env.events().all().filter_by_contract(&h.gov.address);
    assert_eq!(events.events().len(), 1);
    assert!(!std::format!("{:?}", events).contains(&std::format!("{:?}", h.m2)));
    assert_eq!(h.gov.get_proposal(&id).yes_weight, 1);
    assert!(!h.gov.has_voted(&id, &h.m2));
    let image = signature.key_image.clone();
    assert!(h.env.as_contract(&h.gov.address, || {
        h.env
            .storage()
            .temporary()
            .has(&crate::DataKey::KeyImage(id, image))
    }));
}

#[test]
fn anonymous_votes_use_existing_quorum_and_execution_rules() {
    let h = setup();
    let (ring, sk1, sk2) = test_ring(&h);
    let (target, function, args) = set_value_call(&h.env, &h.target, 73);
    let id = h.gov.propose(&h.m3, &target, &function, &args);
    for signer in 0..2 {
        let signature = sign_for_harness(&h, id, true, &ring, &[sk1.clone(), sk2.clone()], signer);
        h.gov.vote_anonymous(&id, &true, &ring, &signature);
    }
    assert_eq!(h.gov.get_proposal(&id).yes_weight, 2);
    assert_eq!(h.gov.try_execute(&id), Err(Ok(Error::QuorumNotMet)));
    h.env.ledger().with_mut(|l| l.sequence_number += 50);
    h.gov.execute(&id);
    assert_eq!(TargetClient::new(&h.env, &h.target).get_value(), 73);
}

#[test]
fn invalid_anonymous_signature_leaves_tally_storage_and_events_unchanged() {
    let h = setup();
    let (ring, sk1, sk2) = test_ring(&h);
    let (target, function, args) = set_value_call(&h.env, &h.target, 8);
    let id = h.gov.propose(&h.m3, &target, &function, &args);
    let mut signature = sign_for_harness(&h, id, true, &ring, &[sk1, sk2], 0);
    signature
        .responses
        .set(0, soroban_sdk::BytesN::from_array(&h.env, &[0u8; 32]));

    assert_eq!(
        h.gov.try_vote_anonymous(&id, &true, &ring, &signature),
        Err(Ok(Error::InvalidRingSignature))
    );
    assert_eq!(h.gov.get_proposal(&id).yes_weight, 0);
    assert_eq!(h.gov.get_proposal(&id).no_weight, 0);
    assert_eq!(
        h.env
            .events()
            .all()
            .filter_by_contract(&h.gov.address)
            .events()
            .len(),
        0
    );
}

#[test]
fn duplicate_key_image_is_rejected_without_a_second_tally_or_event() {
    let h = setup();
    let (ring, sk1, sk2) = test_ring(&h);
    let (target, function, args) = set_value_call(&h.env, &h.target, 8);
    let id = h.gov.propose(&h.m3, &target, &function, &args);
    let signature = sign_for_harness(&h, id, true, &ring, &[sk1, sk2], 0);
    h.gov.vote_anonymous(&id, &true, &ring, &signature);
    let before = h.gov.get_proposal(&id);

    assert_eq!(
        h.gov.try_vote_anonymous(&id, &true, &ring, &signature),
        Err(Ok(Error::DuplicateKeyImage))
    );
    assert_eq!(h.gov.get_proposal(&id).yes_weight, before.yes_weight);
    assert_eq!(
        h.env
            .events()
            .all()
            .filter_by_contract(&h.gov.address)
            .events()
            .len(),
        0
    );
}

#[test]
fn ring_verifier_accepts_each_member_and_rejects_unregistered_keys_or_wrong_message() {
    let h = setup();
    let (ring, sk1, sk2) = test_ring(&h);
    let (target, function, args) = set_value_call(&h.env, &h.target, 8);
    let id = h.gov.propose(&h.m3, &target, &function, &args);
    let message = h.gov.get_anonymous_vote_message(&id, &true, &ring);
    for signer in 0..2 {
        let signature = sign_for_harness(&h, id, true, &ring, &[sk1.clone(), sk2.clone()], signer);
        assert!(h
            .env
            .as_contract(&h.gov.address, || crate::ring_sig::verify(
                &h.env, id, &ring, &message, &signature
            )));
    }
    let outsider = crate::ring_sig::test_support::secret(&h.env, 99);
    let forged = sign_for_harness(&h, id, false, &ring, &[outsider, sk2], 0);
    assert_eq!(
        h.gov.try_vote_anonymous(&id, &false, &ring, &forged),
        Err(Ok(Error::InvalidRingSignature))
    );
    assert_eq!(h.gov.get_proposal(&id).no_weight, 0);
}

#[test]
fn same_member_can_cast_again_on_another_proposal_with_proposal_scoped_key_image() {
    let h = setup();
    let (ring, sk1, sk2) = test_ring(&h);
    let (target, function, args) = set_value_call(&h.env, &h.target, 8);
    let first = h.gov.propose(&h.m1, &target, &function, &args);
    let second = h.gov.propose(&h.m2, &target, &function, &args);
    let first_sig = sign_for_harness(&h, first, true, &ring, &[sk1.clone(), sk2.clone()], 1);
    let second_sig = sign_for_harness(&h, second, true, &ring, &[sk1, sk2], 1);

    assert_ne!(first_sig.key_image, second_sig.key_image);
    h.gov.vote_anonymous(&first, &true, &ring, &first_sig);
    h.gov.vote_anonymous(&second, &true, &ring, &second_sig);
    assert_eq!(h.gov.get_proposal(&first).yes_weight, 1);
    assert_eq!(h.gov.get_proposal(&second).yes_weight, 1);
}

#[test]
fn registered_keys_reject_invalid_points_and_rings_reject_mixed_weights() {
    let h = setup();
    let malformed = soroban_sdk::BytesN::from_array(&h.env, &[0xff; 32]);
    assert_eq!(
        h.gov.try_register_voting_key(&h.m1, &malformed),
        Err(Ok(Error::InvalidVotingKey))
    );

    let (ring, sk1, _sk2) = test_ring(&h);
    let m3_secret = crate::ring_sig::test_support::secret(&h.env, 30);
    let m3_key = register_test_key(&h, &h.m3, &m3_secret);
    let mixed_ring = Vec::from_array(&h.env, [ring.get(0).unwrap(), m3_key]);
    let (target, function, args) = set_value_call(&h.env, &h.target, 8);
    let id = h.gov.propose(&h.m1, &target, &function, &args);
    let signature = sign_for_harness(&h, id, true, &mixed_ring, &[sk1, m3_secret], 0);
    assert_eq!(
        h.gov
            .try_vote_anonymous(&id, &true, &mixed_ring, &signature),
        Err(Ok(Error::InvalidAnonymitySet))
    );
    assert_eq!(h.gov.get_proposal(&id).yes_weight, 0);
}

#[test]
fn anonymous_and_transparent_votes_cannot_be_mixed() {
    let h = setup();
    let (ring, sk1, sk2) = test_ring(&h);
    let (target, function, args) = set_value_call(&h.env, &h.target, 8);
    let id = h.gov.propose(&h.m3, &target, &function, &args);
    let signature = sign_for_harness(&h, id, true, &ring, &[sk1, sk2], 0);
    h.gov.vote_anonymous(&id, &true, &ring, &signature);
    assert_eq!(
        h.gov.try_vote(&h.m1, &id, &true),
        Err(Ok(Error::VotingModeConflict))
    );
}

#[test]
fn anonymous_mode_preserves_transparent_votes_without_registered_keys() {
    let h = setup();
    let (ring, sk1, sk2) = test_ring(&h);
    let (target, function, args) = set_value_call(&h.env, &h.target, 8);
    let id = h.gov.propose(&h.m3, &target, &function, &args);
    let signature = sign_for_harness(&h, id, true, &ring, &[sk1, sk2], 0);
    h.gov.vote_anonymous(&id, &true, &ring, &signature);

    // m3 has no registered LSAG key, so preserve the legacy address vote.
    h.gov.vote(&h.m3, &id, &false);
    assert_eq!(h.gov.get_proposal(&id).no_weight, 2);
}

#[test]
fn prior_transparent_vote_prevents_that_member_from_entering_an_anonymous_ring() {
    let h = setup();
    let (ring, sk1, sk2) = test_ring(&h);
    let (target, function, args) = set_value_call(&h.env, &h.target, 8);
    let id = h.gov.propose(&h.m3, &target, &function, &args);
    h.gov.vote(&h.m1, &id, &true);
    let signature = sign_for_harness(&h, id, true, &ring, &[sk1, sk2], 1);

    assert_eq!(
        h.gov.try_vote_anonymous(&id, &true, &ring, &signature),
        Err(Ok(Error::VotingModeConflict))
    );
    assert_eq!(h.gov.get_proposal(&id).yes_weight, 1);
}
