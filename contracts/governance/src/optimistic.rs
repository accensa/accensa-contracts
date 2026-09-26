//! Optimistic execution for routine DAO tasks (issue #475).
//!
//! Standard [`crate::Governance`] proposals demand quorum + majority across a
//! voting window — appropriate for consequential changes, but slow for the
//! pre-approved, low-risk operations a DAO runs continuously. This module
//! adds an optimistic track: a member queues an operation with
//! [`crate::Governance::optimistic_submit`], it can be executed immediately
//! by anyone, and a supermajority of weight can veto it during a 24-hour
//! challenge window. If the veto threshold is reached, the proposal is locked
//! and its execution reverts.
//!
//! # Rules
//!
//! - **Queue**: each submission allocates the next id and opens an
//!   [`OPTIMISTIC_CHALLENGE_WINDOW`]-ledger challenge window.
//! - **Immediate execution**: any address may
//!   [`execute`](crate::Governance::execute_optimistic) a queued, unvetoed
//!   proposal immediately. Optimistic security is the tradeoff: whoever
//!   executes early accepts that a supermajority veto could still land (an
//!   already-run call cannot be unwound — this track is for operations the
//!   DAO is happy to re-run or that are reversible).
//! - **Veto**: within the window, any member may
//!   [`veto`](crate::Governance::veto_optimistic), contributing their
//!   quadratic weight. Once cumulative veto weight reaches
//!   [`VETO_THRESHOLD_BPS`] (a ~2/3 supermajority) of total weight, the
//!   proposal is vetoed and execution reverts with
//!   [`Error::OptimisticVetoed`].
//! - **Window close**: after the deadline, vetoes close with
//!   [`Error::ChallengeWindowClosed`] and an unvetoed proposal remains
//!   executable.
//! - **Once**: an executed proposal can never execute again
//!   ([`Error::AlreadyExecuted`]); a member vetoes a given proposal once
//!   ([`Error::AlreadyVetoed`]).

use soroban_sdk::{contractevent, contracttype, Address, Env, Symbol, Val, Vec};

use crate::voting::quadratic_weight;
use crate::{DataKey, Error, MAX_THRESHOLD_BPS, PROPOSAL_TTL_GRACE};

/// Length of an optimistic proposal's challenge window, in ledgers: 24 hours
/// at ~5 s/ledger.
pub const OPTIMISTIC_CHALLENGE_WINDOW: u32 = 17_280;

/// Veto threshold, in basis points of total weight: a supermajority (~2/3).
/// `veto_weight * 10_000 >= total_weight * 6_667` vetoes.
pub const VETO_THRESHOLD_BPS: u32 = 6_667;

/// An optimistically queued call plus its running veto tally.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OptimisticProposal {
    pub proposer: Address,
    pub target: Address,
    pub function: Symbol,
    pub args: Vec<Val>,
    /// Last ledger of the challenge window: vetoes close strictly after this.
    pub challenge_deadline_ledger: u32,
    /// Cumulative quadratic weight vetoing the proposal.
    pub veto_weight: u64,
    pub executed: bool,
    pub vetoed: bool,
}

/// Emitted when a member queues an optimistic proposal.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OptimisticQueuedEvent {
    #[topic]
    pub proposal_id: u64,
    pub proposer: Address,
    pub target: Address,
    pub function: Symbol,
    pub challenge_deadline_ledger: u32,
}

/// Emitted on every veto vote.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OptimisticVetoCastEvent {
    #[topic]
    pub proposal_id: u64,
    pub voter: Address,
    pub weight: u64,
    pub veto_weight: u64,
}

/// Emitted the ledger a proposal crosses the supermajority veto threshold.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OptimisticVetoedEvent {
    #[topic]
    pub proposal_id: u64,
}

/// Emitted once a queued optimistic proposal is executed.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OptimisticExecutedEvent {
    #[topic]
    pub proposal_id: u64,
    pub target: Address,
    pub function: Symbol,
}

pub(crate) fn submit(
    env: &Env,
    proposer: &Address,
    target: &Address,
    function: &Symbol,
    args: &Vec<Val>,
) -> Result<u64, Error> {
    proposer.require_auth();
    if quadratic_weight(env, proposer) == 0 {
        return Err(Error::NotAMember);
    }

    let id: u64 = env
        .storage()
        .instance()
        .get(&DataKey::OptimisticCount)
        .unwrap_or(0)
        + 1;
    env.storage().instance().set(&DataKey::OptimisticCount, &id);

    let deadline = env.ledger().sequence() + OPTIMISTIC_CHALLENGE_WINDOW;
    let proposal = OptimisticProposal {
        proposer: proposer.clone(),
        target: target.clone(),
        function: function.clone(),
        args: args.clone(),
        challenge_deadline_ledger: deadline,
        veto_weight: 0,
        executed: false,
        vetoed: false,
    };
    let key = DataKey::OptimisticProposal(id);
    env.storage().persistent().set(&key, &proposal);
    // Threshold == extend_to: a freshly written entry is actually extended
    // past the network's minimum-persistent-TTL floor.
    let ttl = OPTIMISTIC_CHALLENGE_WINDOW.saturating_add(PROPOSAL_TTL_GRACE);
    env.storage().persistent().extend_ttl(&key, ttl, ttl);

    OptimisticQueuedEvent {
        proposal_id: id,
        proposer: proposer.clone(),
        target: target.clone(),
        function: function.clone(),
        challenge_deadline_ledger: deadline,
    }
    .publish(env);

    Ok(id)
}

pub(crate) fn veto(env: &Env, voter: &Address, proposal_id: u64) -> Result<(), Error> {
    voter.require_auth();
    let weight = quadratic_weight(env, voter);
    if weight == 0 {
        return Err(Error::NotAMember);
    }

    let key = DataKey::OptimisticProposal(proposal_id);
    let mut proposal: OptimisticProposal = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(Error::OptimisticNotFound)?;
    if proposal.executed {
        return Err(Error::AlreadyExecuted);
    }
    if proposal.vetoed {
        return Err(Error::OptimisticVetoed);
    }
    let now = env.ledger().sequence();
    if now > proposal.challenge_deadline_ledger {
        return Err(Error::ChallengeWindowClosed);
    }

    let marker = DataKey::OptimisticVeto(proposal_id, voter.clone());
    if env.storage().temporary().has(&marker) {
        return Err(Error::AlreadyVetoed);
    }

    proposal.veto_weight = proposal.veto_weight.saturating_add(weight);
    let total_weight: u64 = env
        .storage()
        .instance()
        .get(&DataKey::TotalWeight)
        .unwrap_or(0);
    let threshold_met = (proposal.veto_weight as u128) * (MAX_THRESHOLD_BPS as u128)
        >= (total_weight as u128) * (VETO_THRESHOLD_BPS as u128);
    if threshold_met {
        proposal.vetoed = true;
    }

    let remaining_ttl = proposal.challenge_deadline_ledger.saturating_sub(now);
    env.storage().temporary().set(&marker, &());
    env.storage()
        .temporary()
        .extend_ttl(&marker, remaining_ttl, remaining_ttl);
    env.storage().persistent().set(&key, &proposal);

    OptimisticVetoCastEvent {
        proposal_id,
        voter: voter.clone(),
        weight,
        veto_weight: proposal.veto_weight,
    }
    .publish(env);
    if threshold_met {
        OptimisticVetoedEvent { proposal_id }.publish(env);
    }

    Ok(())
}

pub(crate) fn execute(env: &Env, proposal_id: u64) -> Result<(), Error> {
    let key = DataKey::OptimisticProposal(proposal_id);
    let mut proposal: OptimisticProposal = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(Error::OptimisticNotFound)?;
    if proposal.executed {
        return Err(Error::AlreadyExecuted);
    }
    if proposal.vetoed {
        return Err(Error::OptimisticVetoed);
    }

    // Effects before interaction: persist `executed = true` before the
    // external call so a reentrant execute is rejected rather than re-run.
    proposal.executed = true;
    env.storage().persistent().set(&key, &proposal);

    let _: Val = env.invoke_contract(&proposal.target, &proposal.function, proposal.args.clone());

    OptimisticExecutedEvent {
        proposal_id,
        target: proposal.target,
        function: proposal.function,
    }
    .publish(env);

    Ok(())
}

pub(crate) fn get(env: &Env, proposal_id: u64) -> Result<OptimisticProposal, Error> {
    env.storage()
        .persistent()
        .get(&DataKey::OptimisticProposal(proposal_id))
        .ok_or(Error::OptimisticNotFound)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::{Governance, GovernanceClient};
    use soroban_sdk::{
        contract, contractimpl, symbol_short,
        testutils::{Address as _, Ledger},
        Address, Env, IntoVal, Symbol, Vec,
    };

    /// Admin-gated target, mirroring the crate's standard test target.
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

    /// Weights 1/1/2 (total 4): the supermajority veto threshold (2/3 of 4 =
    /// 2.67) needs weight 3, i.e. m3 plus one other member. m3 alone (2) or
    /// m1 + m2 (2) fall short.
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
        TargetClient::new(&env, &target_id).init(&gov_id);

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
    fn optimistic_submit_queues_and_executes_immediately() {
        let h = setup();
        let (target, function, args) = set_value_call(&h.env, &h.target, 42);

        let id = h.gov.optimistic_submit(&h.m1, &target, &function, &args);
        assert!(h.gov.get_optimistic_proposal(&id).challenge_deadline_ledger > 0);

        // No votes, no waiting: anyone may execute right away.
        h.gov.execute_optimistic(&id);
        let p = h.gov.get_optimistic_proposal(&id);
        assert!(p.executed);
        assert!(!p.vetoed);
        assert_eq!(TargetClient::new(&h.env, &h.target).get_value(), 42);
    }

    #[test]
    fn supermajority_requires_two_thirds_of_weight() {
        let h = setup();
        let (target, function, args) = set_value_call(&h.env, &h.target, 7);
        let id = h.gov.optimistic_submit(&h.m1, &target, &function, &args);

        // m3 alone (2 of 4 = 50%) falls short of the ~2/3 supermajority.
        h.gov.veto_optimistic(&h.m3, &id);
        assert!(!h.gov.get_optimistic_proposal(&id).vetoed);

        // 2 + 1 weight (3 of 4 = 75%) crosses it.
        h.gov.veto_optimistic(&h.m2, &id);
        assert!(h.gov.get_optimistic_proposal(&id).vetoed);

        assert_eq!(
            h.gov.try_execute_optimistic(&id),
            Err(Ok(Error::OptimisticVetoed))
        );
    }

    #[test]
    fn veto_threshold_crosses_and_execution_reverts() {
        let h = setup();
        let (target, function, args) = set_value_call(&h.env, &h.target, 7);
        let id = h.gov.optimistic_submit(&h.m1, &target, &function, &args);

        // m3 (2) + m1 (1) = 3 of 4 = 75% >= 66.67% -> vetoed.
        h.gov.veto_optimistic(&h.m3, &id);
        h.gov.veto_optimistic(&h.m1, &id);
        assert!(h.gov.get_optimistic_proposal(&id).vetoed);

        assert_eq!(
            h.gov.try_execute_optimistic(&id),
            Err(Ok(Error::OptimisticVetoed))
        );
    }

    #[test]
    fn below_threshold_execution_still_goes_through() {
        let h = setup();
        let (target, function, args) = set_value_call(&h.env, &h.target, 9);
        let id = h.gov.optimistic_submit(&h.m2, &target, &function, &args);

        // m3 (2) vetoes; 2 of 4 = 50% < 66.67% -> still executable.
        h.gov.veto_optimistic(&h.m3, &id);
        assert!(!h.gov.get_optimistic_proposal(&id).vetoed);

        h.gov.execute_optimistic(&id);
        assert!(h.gov.get_optimistic_proposal(&id).executed);
    }

    #[test]
    fn vetoes_close_after_the_challenge_window() {
        let h = setup();
        let (target, function, args) = set_value_call(&h.env, &h.target, 11);
        let id = h.gov.optimistic_submit(&h.m1, &target, &function, &args);

        let deadline = h.gov.get_optimistic_proposal(&id).challenge_deadline_ledger;
        h.env
            .ledger()
            .with_mut(|l| l.sequence_number = deadline + 1);

        assert_eq!(
            h.gov.try_veto_optimistic(&h.m1, &id),
            Err(Ok(Error::ChallengeWindowClosed))
        );

        // Unvetoed, the proposal still executes cleanly.
        h.gov.execute_optimistic(&id);
        assert!(h.gov.get_optimistic_proposal(&id).executed);
    }

    #[test]
    fn veto_rejects_non_member_and_duplicate_vote() {
        let h = setup();
        let (target, function, args) = set_value_call(&h.env, &h.target, 3);
        let id = h.gov.optimistic_submit(&h.m1, &target, &function, &args);

        let outsider = Address::generate(&h.env);
        assert_eq!(
            h.gov.try_veto_optimistic(&outsider, &id),
            Err(Ok(Error::NotAMember))
        );

        h.gov.veto_optimistic(&h.m1, &id);
        assert_eq!(
            h.gov.try_veto_optimistic(&h.m1, &id),
            Err(Ok(Error::AlreadyVetoed))
        );
    }

    #[test]
    fn optimistic_proposal_executes_exactly_once() {
        let h = setup();
        let (target, function, args) = set_value_call(&h.env, &h.target, 5);
        let id = h.gov.optimistic_submit(&h.m1, &target, &function, &args);

        h.gov.execute_optimistic(&id);
        assert_eq!(
            h.gov.try_execute_optimistic(&id),
            Err(Ok(Error::AlreadyExecuted))
        );
        assert_eq!(
            h.gov.try_get_optimistic_proposal(&999),
            Err(Ok(Error::OptimisticNotFound))
        );
    }

    #[test]
    fn non_member_cannot_submit_optimistically() {
        let h = setup();
        let (target, function, args) = set_value_call(&h.env, &h.target, 5);
        let outsider = Address::generate(&h.env);

        assert_eq!(
            h.gov
                .try_optimistic_submit(&outsider, &target, &function, &args),
            Err(Ok(Error::NotAMember))
        );
    }
}
