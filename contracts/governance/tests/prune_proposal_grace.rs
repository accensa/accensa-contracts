//! Integration tests for proposal pruning, voting window, and grace period enforcement (issue #1026).

#![cfg(test)]

use governance::{Error, Governance, GovernanceClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    Address, Env, Symbol, Val, Vec,
};

const VOTING_PERIOD: u32 = 100;
const RAGEQUIT_WINDOW: u32 = 120_960;

struct Harness {
    env: Env,
    gov_id: Address,
    gov: GovernanceClient<'static>,
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

    let gov_id = env.register(Governance, (members, deposits, 5000u32, VOTING_PERIOD));
    let gov = GovernanceClient::new(&env, &gov_id);

    Harness {
        env,
        gov_id,
        gov,
        m1,
        m2,
        m3,
    }
}

#[test]
fn test_prune_approved_proposal_strictly_enforces_active_and_grace_windows() {
    let h = setup();

    let pid = h.gov.propose(
        &h.m3,
        &h.gov_id,
        &Symbol::new(&h.env, "test_call"),
        &Vec::<Val>::new(&h.env),
    );

    // 1. Attempting to prune a live voting proposal returns ProposalActive
    let res = h.gov.try_prune_proposal(&pid);
    assert_eq!(res, Err(Ok(Error::ProposalActive)));

    // Both m2 and m3 vote YES, meeting threshold/quorum and approving the proposal
    h.gov.vote(&h.m2, &pid, &true);
    h.gov.vote(&h.m3, &pid, &true);

    // Advance past voting deadline (deadline is 100 + 100 = 200)
    h.env.ledger().with_mut(|li| li.sequence_number = 205);

    // 2. Approved proposal enters the 7-day ragequit window; pruning during window fails with ProposalActive
    let res_during_rq = h.gov.try_prune_proposal(&pid);
    assert_eq!(res_during_rq, Err(Ok(Error::ProposalActive)));

    // Advance to 1 ledger before ragequit window expires (200 + 120_960)
    h.env
        .ledger()
        .with_mut(|li| li.sequence_number = 200 + RAGEQUIT_WINDOW);
    let res_at_boundary = h.gov.try_prune_proposal(&pid);
    assert_eq!(res_at_boundary, Err(Ok(Error::ProposalActive)));

    // 3. Advance past ragequit window
    h.env
        .ledger()
        .with_mut(|li| li.sequence_number = 200 + RAGEQUIT_WINDOW + 1);

    // Pruning now succeeds and reclaims rent
    let res_prune = h.gov.try_prune_proposal(&pid);
    assert_eq!(res_prune, Ok(Ok(())));

    // 4. Subsequent queries return ProposalNotFound
    let res_get = h.gov.try_get_proposal(&pid);
    assert_eq!(res_get, Err(Ok(Error::ProposalNotFound)));
}

#[test]
fn test_prune_unapproved_proposal_prunes_immediately_after_voting_deadline() {
    let h = setup();

    let pid = h.gov.propose(
        &h.m1,
        &h.gov_id,
        &Symbol::new(&h.env, "test_call"),
        &Vec::<Val>::new(&h.env),
    );

    // Proposal receives no votes; voting deadline is at sequence 200
    // Live proposal rejects pruning
    assert_eq!(
        h.gov.try_prune_proposal(&pid),
        Err(Ok(Error::ProposalActive))
    );

    // Advance ledger past voting deadline (sequence 201)
    h.env.ledger().with_mut(|li| li.sequence_number = 201);

    // Unapproved proposal has no ragequit window; it can be pruned immediately
    assert_eq!(h.gov.try_prune_proposal(&pid), Ok(Ok(())));

    // Verify storage entry was deleted
    assert_eq!(
        h.gov.try_get_proposal(&pid),
        Err(Ok(Error::ProposalNotFound))
    );
}
