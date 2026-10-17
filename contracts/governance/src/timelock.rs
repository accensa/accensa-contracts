//! 48-hour timelock for approved governance proposals (issue #447).
//!
//! Once enabled, an approved proposal cannot run straight away: it has to be
//! **queued** first and may only execute once [`TIMELOCK_DELAY_LEDGERS`]
//! (48 hours) have passed. The delay is the exit window for everyone who
//! disagrees with a contract upgrade or a parameter change.
//!
//! # Flow
//!
//! 1. [`Governance::queue_transaction`]: a member queues a proposal that has
//!    cleared quorum and majority *right now*. The call hash and the ledger at
//!    which execution unlocks are stored.
//! 2. [`Governance::execute_queued_transaction`] (or a plain
//!    [`Governance::execute`], which enforces the same rule): once the unlock
//!    ledger is reached the proposal runs through the normal execution path
//!    and the queue entry is consumed.
//!
//! # Opt-in
//!
//! The timelock is **off** until [`Governance::enable_timelock`] is called, so
//! existing deployments keep their behaviour. There is deliberately no way to
//! switch it back off: a switch-off would be a way around the delay.
//! `enable_timelock` is gated like `set_treasury_token`, by the governance
//! contract's own authorization.
//!
//! # Why this is not `multisig-account`'s timelock
//!
//! `contracts/multisig-account/src/timelock.rs` already queues calls by hash.
//! It is part of that contract's own storage and error enum and of a
//! signer-approval workflow. Governance queues *proposals* that already carry
//! their own vote, so this module reuses the "hash plus unlock ledger" shape,
//! not the code, and adds no dependency on another contract crate.
//!
//! The delay is **not** copied from there: that crate's
//! `DEFAULT_TIMELOCK_DELAY` of `345_600` is documented as 48 hours, but at
//! about five seconds per ledger it is 20 days. 48 hours is `34_560` ledgers.

use soroban_sdk::{contractevent, contractimpl, contracttype, xdr::ToXdr, BytesN, Env};

use crate::quorum::current_quorum_bps;
use crate::{
    DataKey, Error, Governance, GovernanceArgs, GovernanceClient, Proposal, MAX_THRESHOLD_BPS,
};
use soroban_sdk::Address;

/// 48 hours at about five seconds per ledger: `48 * 60 * 60 / 5`.
pub const TIMELOCK_DELAY_LEDGERS: u32 = 34_560;

/// Extra ledgers a queued entry (and its proposal) stay readable after the
/// unlock ledger, so a proposal that matures is still there to execute.
const TIMELOCK_TTL_GRACE: u32 = 17_280;

/// Storage keys of this module. Kept out of [`DataKey`], whose union is
/// close to the XDR spec-union limit.
#[contracttype]
pub enum TimelockKey {
    /// Instance: the timelock has been enabled. Absent means off.
    Enabled,
    /// Persistent: the queue entry of proposal `.0`.
    Queued(u64),
}

/// A proposal waiting out its delay.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueuedTransaction {
    /// SHA-256 of the XDR of `(target, function, args)` at queue time.
    pub call_hash: BytesN<32>,
    /// First ledger at which the proposal may execute.
    pub execution_ledger: u32,
}

/// Emitted once, when the timelock is switched on.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimelockEnabled {}

/// Emitted when a proposal enters the queue.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransactionQueued {
    #[topic]
    pub proposal_id: u64,
    pub call_hash: BytesN<32>,
    pub execution_ledger: u32,
}

fn is_enabled(env: &Env) -> bool {
    env.storage()
        .instance()
        .get(&TimelockKey::Enabled)
        .unwrap_or(false)
}

fn load_queued(env: &Env, proposal_id: u64) -> Option<QueuedTransaction> {
    env.storage()
        .persistent()
        .get(&TimelockKey::Queued(proposal_id))
}

/// SHA-256 of the proposal's call. Binds a queue entry to the exact call the
/// vote approved.
fn call_hash(env: &Env, proposal: &Proposal) -> BytesN<32> {
    let encoded = (
        proposal.target.clone(),
        proposal.function.clone(),
        proposal.args.clone(),
    )
        .to_xdr(env);
    env.crypto().sha256(&encoded).to_bytes()
}

/// Whether `proposal` clears quorum and majority at this ledger.
///
/// This is the test [`Governance::execute`] applies, repeated here so that a
/// proposal can only enter the queue once it is approved: starting the delay
/// before approval would let the notice period run out while voting is still
/// open.
fn is_approved(env: &Env, proposal: &Proposal) -> Result<bool, Error> {
    let total_weight: u64 = env
        .storage()
        .instance()
        .get(&DataKey::TotalWeight)
        .ok_or(Error::InvalidMembers)?;
    let threshold_bps: u32 = env
        .storage()
        .instance()
        .get(&DataKey::ThresholdBps)
        .ok_or(Error::InvalidThreshold)?;
    let voting_period: u32 = env
        .storage()
        .instance()
        .get(&DataKey::VotingPeriod)
        .ok_or(Error::InvalidVotingPeriod)?;

    let created_ledger = proposal.deadline_ledger.saturating_sub(voting_period);
    let elapsed = env.ledger().sequence().saturating_sub(created_ledger);
    let effective_bps = current_quorum_bps(threshold_bps, elapsed, voting_period);

    let quorum_met = (proposal.yes_weight as u128) * (MAX_THRESHOLD_BPS as u128)
        >= (total_weight as u128) * (effective_bps as u128);
    Ok(quorum_met && proposal.yes_weight > proposal.no_weight)
}

/// The gate [`Governance::execute`] calls before it runs a proposal.
///
/// Does nothing while the timelock is off. When it is on, the proposal must
/// be queued, its call must still hash to the queued value, and its unlock
/// ledger must have been reached. The queue entry is consumed here.
pub(crate) fn require_matured(
    env: &Env,
    proposal_id: u64,
    proposal: &Proposal,
) -> Result<(), Error> {
    if !is_enabled(env) {
        return Ok(());
    }
    let queued = load_queued(env, proposal_id).ok_or(Error::TimelockRequired)?;
    if queued.call_hash != call_hash(env, proposal) {
        return Err(Error::TimelockRequired);
    }
    if env.ledger().sequence() < queued.execution_ledger {
        return Err(Error::TimelockNotExpired);
    }
    env.storage()
        .persistent()
        .remove(&TimelockKey::Queued(proposal_id));
    Ok(())
}

#[contractimpl]
impl Governance {
    /// Switch the 48-hour timelock on. Permanent.
    ///
    /// Gated by `current_contract_address().require_auth()`, like
    /// `set_treasury_token`: the host satisfies it only when this contract is
    /// the direct caller, so the timelock is meant to be enabled by an
    /// approved proposal and not by an outside account.
    pub fn enable_timelock(env: Env) -> Result<(), Error> {
        env.current_contract_address().require_auth();
        env.storage().instance().set(&TimelockKey::Enabled, &true);
        TimelockEnabled {}.publish(&env);
        Ok(())
    }

    /// Whether the timelock is on.
    pub fn is_timelock_enabled(env: Env) -> bool {
        is_enabled(&env)
    }

    /// The queue entry of `proposal_id`, if it is queued.
    pub fn get_queued_transaction(env: Env, proposal_id: u64) -> Option<QueuedTransaction> {
        load_queued(&env, proposal_id)
    }

    /// Queue an approved proposal and start its 48-hour delay. Returns the
    /// ledger at which it can execute.
    ///
    /// # Errors
    /// - [`Error::NotAMember`] if `caller` is not a member.
    /// - [`Error::ProposalNotFound`] / [`Error::AlreadyExecuted`] for an
    ///   unknown or finished proposal.
    /// - [`Error::AlreadyQueued`] if it is already in the queue.
    /// - [`Error::QuorumNotMet`] if it has not cleared quorum and majority.
    pub fn queue_transaction(env: Env, caller: Address, proposal_id: u64) -> Result<u32, Error> {
        caller.require_auth();
        Self::member_deposit(&env, &caller)?;

        let key = DataKey::Proposal(proposal_id);
        let proposal: Proposal = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::ProposalNotFound)?;
        if proposal.executed {
            return Err(Error::AlreadyExecuted);
        }
        if load_queued(&env, proposal_id).is_some() {
            return Err(Error::AlreadyQueued);
        }
        if !is_approved(&env, &proposal)? {
            return Err(Error::QuorumNotMet);
        }

        let execution_ledger = env
            .ledger()
            .sequence()
            .saturating_add(TIMELOCK_DELAY_LEDGERS);
        let hash = call_hash(&env, &proposal);
        let queued_key = TimelockKey::Queued(proposal_id);
        env.storage().persistent().set(
            &queued_key,
            &QueuedTransaction {
                call_hash: hash.clone(),
                execution_ledger,
            },
        );

        // The proposal and its queue entry must outlive the delay, or the
        // proposal would be archived before it could execute.
        let ttl = TIMELOCK_DELAY_LEDGERS.saturating_add(TIMELOCK_TTL_GRACE);
        env.storage().persistent().extend_ttl(&queued_key, ttl, ttl);
        env.storage().persistent().extend_ttl(&key, ttl, ttl);

        TransactionQueued {
            proposal_id,
            call_hash: hash,
            execution_ledger,
        }
        .publish(&env);

        Ok(execution_ledger)
    }

    /// Execute a queued proposal once its delay has passed. Runs the normal
    /// [`Governance::execute`] path, which also re-checks quorum.
    ///
    /// # Errors
    /// - [`Error::TimelockRequired`] if the proposal is not queued.
    /// - [`Error::TimelockNotExpired`] before the unlock ledger.
    /// - Anything [`Governance::execute`] returns.
    pub fn execute_queued_transaction(env: Env, proposal_id: u64) -> Result<(), Error> {
        if load_queued(&env, proposal_id).is_none() {
            return Err(Error::TimelockRequired);
        }
        Self::execute(env, proposal_id)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use soroban_sdk::{
        contract, contractimpl, symbol_short,
        testutils::{storage::Persistent as _, Address as _, Ledger},
        Address, IntoVal, Symbol, Val, Vec,
    };

    /// A contract that only the governance contract may change.
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

    /// Three members with deposits 1/1/4 (weights 1/1/2, total 4), 60% quorum
    /// and a 100-ledger voting window. `m3` alone (weight 2) cannot pass;
    /// `m3` plus either other member can.
    fn setup() -> Harness {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.sequence_number = 1_000);

        let m1 = Address::generate(&env);
        let m2 = Address::generate(&env);
        let m3 = Address::generate(&env);
        let members = Vec::from_array(&env, [m1.clone(), m2.clone(), m3.clone()]);
        let deposits = Vec::from_array(&env, [1u64, 1u64, 4u64]);
        let gov_id = env.register(Governance, (members, deposits, 6000u32, 100u32));
        let gov = GovernanceClient::new(&env, &gov_id);

        let target = env.register(Target, ());
        TargetClient::new(&env, &target).init(&gov_id);

        Harness {
            env,
            gov,
            target,
            m1,
            m2,
            m3,
        }
    }

    fn set_value_args(env: &Env, value: u32) -> Vec<Val> {
        Vec::from_array(env, [value.into_val(env)])
    }

    /// Propose `set_value(value)` and pass it with `m3` + `m1` (3 of 4).
    fn approved_proposal(h: &Harness, value: u32) -> u64 {
        let args = set_value_args(&h.env, value);
        let id = h
            .gov
            .propose(&h.m3, &h.target, &Symbol::new(&h.env, "set_value"), &args);
        h.gov.vote(&h.m3, &id, &true);
        h.gov.vote(&h.m1, &id, &true);
        id
    }

    fn now(h: &Harness) -> u32 {
        h.env.ledger().sequence()
    }

    fn advance_to(h: &Harness, sequence: u32) {
        h.env.ledger().with_mut(|l| l.sequence_number = sequence);
    }

    #[test]
    fn delay_is_forty_eight_hours_of_five_second_ledgers() {
        assert_eq!(TIMELOCK_DELAY_LEDGERS, 48 * 60 * 60 / 5);
    }

    #[test]
    fn timelock_is_off_by_default_and_execution_is_unchanged() {
        let h = setup();
        assert!(!h.gov.is_timelock_enabled());
        let id = approved_proposal(&h, 7);
        // No queue step is needed while the timelock is off.
        h.gov.execute(&id);
        assert_eq!(TargetClient::new(&h.env, &h.target).get_value(), 7);
    }

    #[test]
    fn enabled_timelock_refuses_execution_of_an_unqueued_proposal() {
        let h = setup();
        h.gov.enable_timelock();
        assert!(h.gov.is_timelock_enabled());

        let id = approved_proposal(&h, 7);
        assert_eq!(h.gov.try_execute(&id), Err(Ok(Error::TimelockRequired)));
        assert_eq!(
            h.gov.try_execute_queued_transaction(&id),
            Err(Ok(Error::TimelockRequired))
        );
        assert_eq!(TargetClient::new(&h.env, &h.target).get_value(), 0);
    }

    #[test]
    fn queue_records_the_hash_and_the_unlock_ledger() {
        let h = setup();
        h.gov.enable_timelock();
        let id = approved_proposal(&h, 7);

        let queued_at = now(&h);
        let unlock = h.gov.queue_transaction(&h.m1, &id);
        assert_eq!(unlock, queued_at + TIMELOCK_DELAY_LEDGERS);

        let entry = h.gov.get_queued_transaction(&id).unwrap();
        assert_eq!(entry.execution_ledger, unlock);
        let proposal = h.gov.get_proposal(&id);
        h.env.as_contract(&h.gov.address, || {
            assert_eq!(entry.call_hash, call_hash(&h.env, &proposal));
        });
    }

    #[test]
    fn early_execution_reverts_and_the_exact_unlock_ledger_succeeds() {
        let h = setup();
        h.gov.enable_timelock();
        let id = approved_proposal(&h, 42);
        let unlock = h.gov.queue_transaction(&h.m1, &id);

        // Right away, and one ledger before the unlock ledger.
        for ledger in [now(&h), unlock - 1] {
            advance_to(&h, ledger);
            assert_eq!(h.gov.try_execute(&id), Err(Ok(Error::TimelockNotExpired)));
            assert_eq!(
                h.gov.try_execute_queued_transaction(&id),
                Err(Ok(Error::TimelockNotExpired))
            );
        }
        assert_eq!(TargetClient::new(&h.env, &h.target).get_value(), 0);
        assert!(h.gov.get_queued_transaction(&id).is_some(), "still queued");

        // At the unlock ledger the delayed call runs.
        advance_to(&h, unlock);
        h.gov.execute_queued_transaction(&id);
        assert_eq!(TargetClient::new(&h.env, &h.target).get_value(), 42);
        assert!(h.gov.get_proposal(&id).executed);
        assert!(
            h.gov.get_queued_transaction(&id).is_none(),
            "entry consumed"
        );
        assert_eq!(h.gov.try_execute(&id), Err(Ok(Error::AlreadyExecuted)));
    }

    #[test]
    fn plain_execute_also_honours_a_matured_queue_entry() {
        let h = setup();
        h.gov.enable_timelock();
        let id = approved_proposal(&h, 5);
        let unlock = h.gov.queue_transaction(&h.m2, &id);
        advance_to(&h, unlock + 1_000);
        h.gov.execute(&id);
        assert_eq!(TargetClient::new(&h.env, &h.target).get_value(), 5);
    }

    #[test]
    fn a_proposal_below_quorum_cannot_be_queued() {
        let h = setup();
        h.gov.enable_timelock();
        let args = set_value_args(&h.env, 1);
        let id = h
            .gov
            .propose(&h.m3, &h.target, &Symbol::new(&h.env, "set_value"), &args);
        // Only m3 votes: weight 2 of 4 is 50%, below the 60% quorum.
        h.gov.vote(&h.m3, &id, &true);

        assert_eq!(
            h.gov.try_queue_transaction(&h.m3, &id),
            Err(Ok(Error::QuorumNotMet))
        );
        // `execute` rejects the same proposal for the same reason, before it
        // ever reaches the timelock gate.
        assert_eq!(h.gov.try_execute(&id), Err(Ok(Error::QuorumNotMet)));
        assert!(h.gov.get_queued_transaction(&id).is_none());
    }

    #[test]
    fn a_proposal_that_is_voted_down_cannot_be_queued() {
        let h = setup();
        let args = set_value_args(&h.env, 2);
        let id = h
            .gov
            .propose(&h.m1, &h.target, &Symbol::new(&h.env, "set_value"), &args);
        // 2 "no" against 1 "yes": no majority.
        h.gov.vote(&h.m3, &id, &false);
        h.gov.vote(&h.m1, &id, &true);
        assert_eq!(
            h.gov.try_queue_transaction(&h.m1, &id),
            Err(Ok(Error::QuorumNotMet))
        );
    }

    #[test]
    fn only_a_member_can_queue_and_only_once() {
        let h = setup();
        let id = approved_proposal(&h, 3);

        let outsider = Address::generate(&h.env);
        assert_eq!(
            h.gov.try_queue_transaction(&outsider, &id),
            Err(Ok(Error::NotAMember))
        );
        assert_eq!(
            h.gov.try_queue_transaction(&h.m1, &99),
            Err(Ok(Error::ProposalNotFound))
        );

        h.gov.queue_transaction(&h.m1, &id);
        assert_eq!(
            h.gov.try_queue_transaction(&h.m2, &id),
            Err(Ok(Error::AlreadyQueued))
        );
    }

    #[test]
    fn an_executed_proposal_cannot_be_queued() {
        let h = setup();
        let id = approved_proposal(&h, 3);
        h.gov.execute(&id);
        assert_eq!(
            h.gov.try_queue_transaction(&h.m1, &id),
            Err(Ok(Error::AlreadyExecuted))
        );
    }

    #[test]
    fn queueing_extends_the_ttl_of_the_proposal_and_its_entry_past_the_delay() {
        // A proposal's own TTL covers its voting and ragequit windows,
        // 121,160 ledgers from creation. Queue it close to the end of that
        // and the 34,560-ledger delay runs past it, so the proposal would be
        // archived before it could execute unless queueing extends it.
        // (Unit tests do not enforce expiry, so the stored TTLs are read.)
        let h = setup();
        h.gov.enable_timelock();
        let id = approved_proposal(&h, 11);
        let created = now(&h);
        advance_to(&h, created + 120_000);

        let before = h.env.as_contract(&h.gov.address, || {
            h.env.storage().persistent().get_ttl(&DataKey::Proposal(id))
        });
        assert!(
            before < TIMELOCK_DELAY_LEDGERS,
            "the proposal is about to expire"
        );

        let unlock = h.gov.queue_transaction(&h.m1, &id);
        let (proposal_ttl, entry_ttl) = h.env.as_contract(&h.gov.address, || {
            (
                h.env.storage().persistent().get_ttl(&DataKey::Proposal(id)),
                h.env
                    .storage()
                    .persistent()
                    .get_ttl(&TimelockKey::Queued(id)),
            )
        });
        let needed = unlock - now(&h) + TIMELOCK_TTL_GRACE;
        assert!(
            proposal_ttl >= needed,
            "proposal outlives the delay: {proposal_ttl} < {needed}"
        );
        assert!(
            entry_ttl >= needed,
            "queue entry outlives the delay: {entry_ttl} < {needed}"
        );

        advance_to(&h, unlock);
        h.gov.execute_queued_transaction(&id);
        assert_eq!(TargetClient::new(&h.env, &h.target).get_value(), 11);
    }

    #[test]
    fn queueing_emits_exactly_one_governance_event() {
        use soroban_sdk::testutils::Events as _;
        let h = setup();
        let id = approved_proposal(&h, 1);
        h.gov.queue_transaction(&h.m1, &id);
        let events = h.env.events().all().filter_by_contract(&h.gov.address);
        assert_eq!(events.events().len(), 1);
    }
}
