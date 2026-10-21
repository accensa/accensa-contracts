//! Event-shape coverage for optimistic execution (issue #475) and the
//! standard proposal/ragequit trail (issue #411).
//!
//! Every event is compared in the same XDR form the other governance and
//! state-channel event tests use. Member actions use explicit `mock_auths`
//! entries, never `mock_all_auths`, so each test proves the signer actually
//! authorized the call. `events().all()` reflects the **last invocation only**;
//! each assertion below reads it immediately after the call that emits it.

use governance::{
    optimistic::{
        OptimisticExecutedEvent, OptimisticQueuedEvent, OptimisticVetoCastEvent,
        OptimisticVetoedEvent, OPTIMISTIC_CHALLENGE_WINDOW,
    },
    Error, Governance, GovernanceClient, ProposalExecutedEvent, RagequitEvent,
};
use soroban_sdk::{
    contract, contractimpl, symbol_short,
    testutils::{Address as _, Events as _, MockAuth, MockAuthInvoke},
    token::{StellarAssetClient, TokenClient},
    Address, Env, Event, IntoVal, Symbol, Val, Vec,
};

/// Auth-free target: no target-side `require_auth` should mask the governance
/// event being tested.
#[contract]
struct Target;

#[contractimpl]
impl Target {
    pub fn noop(_env: Env) {}

    pub fn set_value(env: Env, value: u32) {
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
    gov_id: Address,
    target: Address,
    m1: Address,
    m2: Address,
    m3: Address,
}

/// Weights 1/1/2 (total 4): the ~2/3 veto threshold needs weight 3, so m3
/// plus one other member vetoes while m3 alone does not. A 60% quorum lets
/// m3 + m1 approve a standard proposal over m2's dissent.
fn setup() -> Harness {
    let env = Env::default();

    let m1 = Address::generate(&env);
    let m2 = Address::generate(&env);
    let m3 = Address::generate(&env);

    let members = Vec::from_array(&env, [m1.clone(), m2.clone(), m3.clone()]);
    let deposits = Vec::from_array(&env, [1u64, 1u64, 4u64]);
    let gov_id = env.register(Governance, (members, deposits, 6_000u32, 100u32));
    let gov = GovernanceClient::new(&env, &gov_id);
    let target = env.register(Target, ());

    Harness {
        env,
        gov,
        gov_id,
        target,
        m1,
        m2,
        m3,
    }
}

/// Install exactly one authorization entry for `address` invoking
/// `contract::fn_name(args)`.
fn mock_auth(env: &Env, address: &Address, contract: &Address, fn_name: &str, args: Vec<Val>) {
    env.mock_auths(&[MockAuth {
        address,
        invoke: &MockAuthInvoke {
            contract,
            fn_name,
            args,
            sub_invokes: &[],
        },
    }]);
}

fn mock_optimistic_submit_auth(
    h: &Harness,
    proposer: &Address,
    function: &Symbol,
    call_args: &Vec<Val>,
) {
    let invoke_args = soroban_sdk::vec![
        &h.env,
        proposer.into_val(&h.env),
        h.target.into_val(&h.env),
        function.into_val(&h.env),
        call_args.into_val(&h.env),
    ];
    mock_auth(
        &h.env,
        proposer,
        &h.gov_id,
        "optimistic_submit",
        invoke_args,
    );
}

fn mock_veto_auth(h: &Harness, voter: &Address, proposal_id: u64) {
    let invoke_args =
        soroban_sdk::vec![&h.env, voter.into_val(&h.env), proposal_id.into_val(&h.env)];
    mock_auth(&h.env, voter, &h.gov_id, "veto_optimistic", invoke_args);
}

fn mock_propose_auth(h: &Harness, proposer: &Address, function: &Symbol, call_args: &Vec<Val>) {
    let invoke_args = soroban_sdk::vec![
        &h.env,
        proposer.into_val(&h.env),
        h.target.into_val(&h.env),
        function.into_val(&h.env),
        call_args.into_val(&h.env),
    ];
    mock_auth(&h.env, proposer, &h.gov_id, "propose", invoke_args);
}

fn mock_vote_auth(h: &Harness, voter: &Address, proposal_id: u64, support: bool) {
    let invoke_args = soroban_sdk::vec![
        &h.env,
        voter.into_val(&h.env),
        proposal_id.into_val(&h.env),
        support.into_val(&h.env),
    ];
    mock_auth(&h.env, voter, &h.gov_id, "vote", invoke_args);
}

fn mock_ragequit_auth(h: &Harness, voter: &Address, proposal_id: u64) {
    let invoke_args =
        soroban_sdk::vec![&h.env, voter.into_val(&h.env), proposal_id.into_val(&h.env)];
    mock_auth(&h.env, voter, &h.gov_id, "ragequit", invoke_args);
}

fn submitted_optimistic(h: &Harness, value: u32) -> (u64, Symbol, Vec<Val>) {
    let function = Symbol::new(&h.env, "set_value");
    let args = Vec::from_array(&h.env, [value.into_val(&h.env)]);
    mock_optimistic_submit_auth(h, &h.m1, &function, &args);
    let id = h.gov.optimistic_submit(&h.m1, &h.target, &function, &args);
    (id, function, args)
}

#[test]
fn optimistic_submit_emits_the_queued_event_with_a_full_payload() {
    let h = setup();
    let (id, function, _args) = submitted_optimistic(&h, 42);

    let events = h.env.events().all().filter_by_contract(&h.gov_id);
    assert_eq!(events.events().len(), 1, "queued event only");
    let expected = OptimisticQueuedEvent {
        proposal_id: id,
        proposer: h.m1.clone(),
        target: h.target.clone(),
        function,
        challenge_deadline_ledger: h.env.ledger().sequence() + OPTIMISTIC_CHALLENGE_WINDOW,
    };
    assert_eq!(&events.events()[0], &expected.to_xdr(&h.env, &h.gov_id));
}

#[test]
fn veto_casts_cross_the_threshold_and_block_execution() {
    let h = setup();
    let (id, _function, _args) = submitted_optimistic(&h, 7);

    // m1 alone (weight 1 of 4) is below the ~2/3 veto supermajority.
    mock_veto_auth(&h, &h.m1, id);
    h.gov.veto_optimistic(&h.m1, &id);

    let events = h.env.events().all().filter_by_contract(&h.gov_id);
    assert_eq!(events.events().len(), 1, "veto cast only");
    let expected_cast = OptimisticVetoCastEvent {
        proposal_id: id,
        voter: h.m1.clone(),
        weight: 1,
        veto_weight: 1,
    };
    assert_eq!(
        &events.events()[0],
        &expected_cast.to_xdr(&h.env, &h.gov_id)
    );

    // m3 (weight 2) brings the total to 3 of 4 (75%), crossing the threshold.
    mock_veto_auth(&h, &h.m3, id);
    h.gov.veto_optimistic(&h.m3, &id);

    let events = h.env.events().all().filter_by_contract(&h.gov_id);
    assert_eq!(events.events().len(), 2, "cast + threshold-crossing event");
    let expected_cast = OptimisticVetoCastEvent {
        proposal_id: id,
        voter: h.m3.clone(),
        weight: 2,
        veto_weight: 3,
    };
    let expected_vetoed = OptimisticVetoedEvent { proposal_id: id };
    assert_eq!(
        &events.events()[0],
        &expected_cast.to_xdr(&h.env, &h.gov_id)
    );
    assert_eq!(
        &events.events()[1],
        &expected_vetoed.to_xdr(&h.env, &h.gov_id)
    );

    // A vetoed proposal can never execute: the call returns the typed error,
    // emits no OptimisticExecutedEvent, and leaves the target untouched.
    h.env.set_auths(&[]);
    assert_eq!(
        h.gov.try_execute_optimistic(&id),
        Err(Ok(Error::OptimisticVetoed))
    );
    assert!(
        h.env
            .events()
            .all()
            .filter_by_contract(&h.gov_id)
            .events()
            .is_empty(),
        "a refused execution must emit no OptimisticExecutedEvent"
    );
    assert_eq!(TargetClient::new(&h.env, &h.target).get_value(), 0);
}

#[test]
fn optimistic_execution_emits_the_executed_event_and_runs_the_target() {
    let h = setup();
    let (id, function, _args) = submitted_optimistic(&h, 42);

    h.env.set_auths(&[]);
    h.gov.execute_optimistic(&id);

    let events = h.env.events().all().filter_by_contract(&h.gov_id);
    assert_eq!(events.events().len(), 1, "executed event only");
    let expected = OptimisticExecutedEvent {
        proposal_id: id,
        target: h.target.clone(),
        function,
    };
    assert_eq!(&events.events()[0], &expected.to_xdr(&h.env, &h.gov_id));
    assert_eq!(TargetClient::new(&h.env, &h.target).get_value(), 42);
}

#[test]
fn proposal_executed_and_ragequit_events_carry_the_full_payload() {
    let h = setup();

    // Fund a treasury and configure it as the payout token. The issuer
    // authorizes `mint` and governance authorizes the self-gated setter.
    let issuer = Address::generate(&h.env);
    let sac = h.env.register_stellar_asset_contract_v2(issuer.clone());
    let token = sac.address();
    let mint_args = soroban_sdk::vec![&h.env, h.gov_id.into_val(&h.env), 600i128.into_val(&h.env),];
    mock_auth(&h.env, &issuer, &token, "mint", mint_args);
    StellarAssetClient::new(&h.env, &token).mint(&h.gov_id, &600i128);

    // `set_treasury_token` authorizes via the governance contract's own
    // address. An explicit `mock_auths` entry cannot express that: it would
    // register the mock-auth contract *at* the governance address. Use
    // mock-all for this setup call only; the next `mock_auths` restores
    // explicit authorization for every member action below.
    h.env.mock_all_auths();
    h.gov.set_treasury_token(&token);

    // Standard proposal: m3 + m1 approve (3 of 4 = 75%), m2 dissents.
    let function = Symbol::new(&h.env, "noop");
    let call_args = Vec::new(&h.env);
    mock_propose_auth(&h, &h.m1, &function, &call_args);
    let id = h.gov.propose(&h.m1, &h.target, &function, &call_args);

    mock_vote_auth(&h, &h.m3, id, true);
    h.gov.vote(&h.m3, &id, &true);
    mock_vote_auth(&h, &h.m1, id, true);
    h.gov.vote(&h.m1, &id, &true);
    mock_vote_auth(&h, &h.m2, id, false);
    h.gov.vote(&h.m2, &id, &false);

    // m2 ragequits before execution: 600 * 1 / 6 = 100 tokens. Its weight is
    // 1, so the event's `deposit_burned` and the total-weight drop must both
    // reflect exactly this member.
    let member_deposit = h.gov.get_member_deposit(&h.m2);
    let member_weight = h.gov.get_member_weight(&h.m2);
    assert_eq!(member_deposit, 1);
    assert_eq!(member_weight, 1);
    assert_eq!(h.gov.get_total_weight(), 4);

    mock_ragequit_auth(&h, &h.m2, id);
    h.gov.ragequit(&h.m2, &id);

    let events = h.env.events().all().filter_by_contract(&h.gov_id);
    assert_eq!(events.events().len(), 1, "ragequit event only");
    let expected = RagequitEvent {
        proposal_id: id,
        voter: h.m2.clone(),
        deposit_burned: member_deposit,
        payout: 100,
    };
    assert_eq!(&events.events()[0], &expected.to_xdr(&h.env, &h.gov_id));

    assert_eq!(h.gov.get_total_weight(), 4 - member_weight);
    assert_eq!(h.gov.get_quarantined(&h.m2), member_deposit);
    let sac_client = TokenClient::new(&h.env, &token);
    assert_eq!(sac_client.balance(&h.m2), 100);
    assert_eq!(sac_client.balance(&h.gov_id), 500);

    // Execution is permissionless: no auth entries at all.
    h.env.set_auths(&[]);
    h.gov.execute(&id);

    let events = h.env.events().all().filter_by_contract(&h.gov_id);
    assert_eq!(events.events().len(), 1, "proposal-executed event only");
    let expected = ProposalExecutedEvent {
        proposal_id: id,
        target: h.target.clone(),
        function,
    };
    assert_eq!(&events.events()[0], &expected.to_xdr(&h.env, &h.gov_id));
}
