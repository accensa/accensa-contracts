//! Unit tests for [`Governance`], including a minimal target contract that
//! proves `execute` authorizes a governed call the same way `ReceiptAnchor`
//! expects its admin to (see `tests/receipt_anchor_admin.rs` for the same
//! proof against the real contract).

extern crate std;

use crate::{Error, Governance, GovernanceClient};
use soroban_sdk::{
    contract, contractimpl, symbol_short,
    testutils::{Address as _, Ledger},
    Address, Env, IntoVal, Symbol, Val, Vec,
};

/// A trivial admin-gated contract: `set_value` requires the stored admin's
/// auth, exactly like `ReceiptAnchor::set_min_anchor_interval` requires its
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

/// Three members weighted 1/1/2 (total 4), 6000 bps (60%) quorum, 100-ledger
/// voting window. `m3` alone (weight 2) cannot pass; `m3` + either other
/// member (weight 3) can.
fn setup() -> Harness {
    let env = Env::default();
    env.mock_all_auths();

    let m1 = Address::generate(&env);
    let m2 = Address::generate(&env);
    let m3 = Address::generate(&env);

    let members = Vec::from_array(&env, [m1.clone(), m2.clone(), m3.clone()]);
    let weights = Vec::from_array(&env, [1u64, 1u64, 2u64]);

    let gov_id = env.register(Governance, (members, weights, 6000u32, 100u32));
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
    let weights = Vec::from_array(&env, [1u64, 2u64]);
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.register(Governance, (members, weights, 5000u32, 10u32))
    }));
    assert!(
        res.is_err(),
        "mismatched members/weights must reject construction"
    );
}

#[test]
fn constructor_rejects_zero_threshold() {
    let env = Env::default();
    let m1 = Address::generate(&env);
    let members = Vec::from_array(&env, [m1]);
    let weights = Vec::from_array(&env, [1u64]);
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.register(Governance, (members, weights, 0u32, 10u32))
    }));
    assert!(res.is_err(), "a zero threshold must reject construction");
}

#[test]
fn constructor_rejects_zero_voting_period() {
    let env = Env::default();
    let m1 = Address::generate(&env);
    let members = Vec::from_array(&env, [m1]);
    let weights = Vec::from_array(&env, [1u64]);
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.register(Governance, (members, weights, 5000u32, 0u32))
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
