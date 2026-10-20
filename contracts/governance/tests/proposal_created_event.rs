//! `ProposalCreated` payload coverage (issue #774).
//!
//! The five public fields are documented on the struct in
//! `contracts/governance/src/lib.rs` so an off-chain decoder can read each
//! value correctly. These tests pin what the documented creating path actually
//! publishes, in the same XDR form the other governance event tests use:
//! `proposal_id` is the incremented `DataKey::ProposalCount` and the event
//! topic, `proposer`/`target`/`function` are the submitted call, and
//! `deadline_ledger` is the creation ledger plus the configured voting period.

use governance::{Error, Governance, GovernanceClient, ProposalCreated};
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Events as _, Ledger as _, MockAuth, MockAuthInvoke},
    Address, Env, Event, IntoVal, Symbol, Val, Vec,
};

/// Auth-free target: nothing on the target side may mask the governance event.
#[contract]
struct Target;

#[contractimpl]
impl Target {
    pub fn noop(_env: Env) {}
}

struct Harness {
    env: Env,
    gov: GovernanceClient<'static>,
    gov_id: Address,
    target: Address,
    m1: Address,
    m2: Address,
}

/// Weights 1 and 2; a 100-ledger voting window.
fn setup() -> Harness {
    let env = Env::default();
    let m1 = Address::generate(&env);
    let m2 = Address::generate(&env);
    let members = Vec::from_array(&env, [m1.clone(), m2.clone()]);
    let deposits = Vec::from_array(&env, [1u64, 4u64]);
    let gov_id = env.register(Governance, (members, deposits, 5_000u32, 100u32));
    let gov = GovernanceClient::new(&env, &gov_id);
    let target = env.register(Target, ());

    Harness {
        env,
        gov,
        gov_id,
        target,
        m1,
        m2,
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

fn propose(h: &Harness, function: &Symbol, call_args: &Vec<Val>) -> u64 {
    let invoke_args = soroban_sdk::vec![
        &h.env,
        h.m1.into_val(&h.env),
        h.target.into_val(&h.env),
        function.into_val(&h.env),
        call_args.into_val(&h.env),
    ];
    mock_auth(&h.env, &h.m1, &h.gov_id, "propose", invoke_args);
    h.gov.propose(&h.m1, &h.target, function, call_args)
}

fn vote(h: &Harness, voter: &Address, proposal_id: u64, support: bool) {
    let invoke_args = soroban_sdk::vec![
        &h.env,
        voter.into_val(&h.env),
        proposal_id.into_val(&h.env),
        support.into_val(&h.env),
    ];
    mock_auth(&h.env, voter, &h.gov_id, "vote", invoke_args);
    h.gov.vote(voter, &proposal_id, &support);
}

#[test]
fn propose_emits_proposal_created_with_the_documented_field_values() {
    let h = setup();
    let function = Symbol::new(&h.env, "noop");
    let call_args = Vec::new(&h.env);

    let voting_period = h.gov.get_voting_period();
    let creation_ledger = h.env.ledger().sequence();
    let id = propose(&h, &function, &call_args);

    // `proposal_id` is the incremented `DataKey::ProposalCount`, starting at 1.
    assert_eq!(id, 1);

    // `events().all()` reflects the last invocation only, so read it before
    // any further contract call.
    let events = h.env.events().all().filter_by_contract(&h.gov_id);
    assert_eq!(events.events().len(), 1, "proposal-created event only");
    let expected = ProposalCreated {
        proposal_id: id,
        proposer: h.m1.clone(),
        target: h.target.clone(),
        function: function.clone(),
        deadline_ledger: creation_ledger + voting_period,
    };
    assert_eq!(&events.events()[0], &expected.to_xdr(&h.env, &h.gov_id));

    // The published id keys the stored proposal with the same calldata, and
    // the stored counter advanced with it.
    assert_eq!(id, h.gov.get_proposal_count());
    let stored = h.gov.get_proposal(&id);
    assert_eq!(stored.proposer, h.m1);
    assert_eq!(stored.target, h.target);
    assert_eq!(stored.function, function);
    assert_eq!(stored.deadline_ledger, expected.deadline_ledger);
}

#[test]
fn deadline_ledger_is_the_last_ledger_that_accepts_a_vote() {
    let h = setup();
    let function = Symbol::new(&h.env, "noop");
    let call_args = Vec::new(&h.env);
    let id = propose(&h, &function, &call_args);
    let deadline = h.gov.get_proposal(&id).deadline_ledger;

    // The same ledger as the deadline is still inside the window.
    h.env.ledger().with_mut(|l| l.sequence_number = deadline);
    vote(&h, &h.m1, id, true);

    // One ledger later the window is closed.
    h.env
        .ledger()
        .with_mut(|l| l.sequence_number = deadline + 1);
    let invoke_args = soroban_sdk::vec![
        &h.env,
        h.m2.into_val(&h.env),
        id.into_val(&h.env),
        true.into_val(&h.env),
    ];
    mock_auth(&h.env, &h.m2, &h.gov_id, "vote", invoke_args);
    assert_eq!(
        h.gov.try_vote(&h.m2, &id, &true),
        Err(Ok(Error::VotingClosed))
    );
}
