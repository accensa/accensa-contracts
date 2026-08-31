//! A proposal-based, weighted-voting governance contract for Soroban admin
//! roles (issue: single-admin-key SPOF on `ReceiptAnchor`).
//!
//! Unlike [`multisig_account`](https://github.com/accensa/accensa-contracts) —
//! which aggregates signatures within one transaction via
//! `CustomAccountInterface` — this contract is a plain contract that carries
//! a proposal's approval across *multiple* transactions: a member proposes a
//! call, members cast weighted votes over a bounded voting window, and once
//! the "yes" weight clears the configured quorum (and outweighs "no"),
//! anyone may execute it.
//!
//! Execution requires **no change to the governed contract**. When
//! [`Governance::execute`] calls into a target contract that in turn does
//! `governance_address.require_auth()`, the host authorizes it automatically
//! — a contract's `require_auth()` on its own address always succeeds when
//! the direct caller of the current invocation *is* that same contract. This
//! is the same mechanism that already lets `ReceiptAnchor`'s merchant admin
//! be a contract address (see `multisig-account`); it requires nothing
//! special from `ReceiptAnchor` beyond `initialize`-ing it with this
//! contract's address as `merchant`.
//!
//! # Storage shape (kept deliberately small)
//!
//! - Each member's weight is its own persistent entry (`Member(Address)`),
//!   so casting a vote or checking membership never touches a shared blob.
//! - A proposal (`Proposal(id)`) carries its calldata and running tally.
//!   Its TTL is bounded to its voting window plus a small grace period —
//!   proposals are inherently short-lived, unlike `ReceiptAnchor`'s
//!   permanently-retained batches — and [`Governance::prune_proposal`] lets
//!   anyone reclaim a resolved proposal's rent immediately rather than
//!   waiting on archival.
//! - A "did this address vote" marker (`Voted(id, Address)`) lives in
//!   **temporary** storage, so per-voter state never accumulates: it expires
//!   with the voting window on its own, with no cleanup logic needed.

#![no_std]

#[cfg(test)]
mod test;

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contractmeta, contracttype, Address, Env,
    Symbol, Val, Vec,
};

contractmeta!(key = "name", val = "Governance");
contractmeta!(key = "version", val = env!("CARGO_PKG_VERSION"));
contractmeta!(
    key = "repo",
    val = "https://github.com/accensa/accensa-contracts"
);

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    /// `__constructor` received `members`/`weights` of different lengths.
    ArityMismatch = 1,
    /// `__constructor` received zero members, more than `MAX_MEMBERS`, a
    /// duplicate member, a member with zero weight, or a weight sum that
    /// overflows `u64`.
    InvalidMembers = 2,
    /// `threshold_bps` was `0` or greater than `10_000`.
    InvalidThreshold = 3,
    /// `voting_period_ledgers` was `0`.
    InvalidVotingPeriod = 4,
    /// The caller is not a registered member.
    NotAMember = 5,
    /// No proposal exists with the given id (or it has been pruned).
    ProposalNotFound = 6,
    /// The proposal's voting window has closed.
    VotingClosed = 7,
    /// The caller already voted on this proposal.
    AlreadyVoted = 8,
    /// The proposal was already executed.
    AlreadyExecuted = 9,
    /// "Yes" weight has not cleared quorum, or does not strictly exceed
    /// "no" weight.
    QuorumNotMet = 10,
    /// `prune_proposal` was called on a proposal still inside its voting
    /// window and not yet executed.
    ProposalActive = 11,
}

#[contracttype]
pub enum DataKey {
    /// Instance: sum of every member's weight.
    TotalWeight,
    /// Instance: quorum, in basis points (`1..=10_000`) of `TotalWeight`.
    ThresholdBps,
    /// Instance: length of a proposal's voting window, in ledgers.
    VotingPeriod,
    /// Instance: number of proposals ever created; also the next id.
    ProposalCount,
    /// Persistent, one entry per member: that member's voting weight.
    Member(Address),
    /// Persistent: a proposal's calldata and running tally.
    Proposal(u64),
    /// Temporary: marks that `.1` already voted on proposal `.0`.
    Voted(u64, Address),
}

/// A proposed call plus its running weighted tally.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Proposal {
    pub proposer: Address,
    pub target: Address,
    pub function: Symbol,
    pub args: Vec<Val>,
    pub yes_weight: u64,
    pub no_weight: u64,
    pub deadline_ledger: u32,
    pub executed: bool,
}

/// Emitted when a member creates a proposal.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProposalCreated {
    #[topic]
    pub proposal_id: u64,
    pub proposer: Address,
    pub target: Address,
    pub function: Symbol,
    pub deadline_ledger: u32,
}

/// Emitted on every vote.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VoteCast {
    #[topic]
    pub proposal_id: u64,
    pub voter: Address,
    pub support: bool,
    pub weight: u64,
}

/// Emitted once a proposal is executed.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProposalExecutedEvent {
    #[topic]
    pub proposal_id: u64,
    pub target: Address,
    pub function: Symbol,
}

/// Upper bound on registered members, so `__constructor` and per-member
/// operations stay bounded-cost. A governance body for an admin role is
/// expected to be small; raise this deliberately if that changes.
const MAX_MEMBERS: u32 = 32;

const MIN_THRESHOLD_BPS: u32 = 1;
const MAX_THRESHOLD_BPS: u32 = 10_000;

/// Grace period (in ledgers) past a proposal's deadline before its storage
/// TTL lapses, so a same-ledger `execute` at the deadline still finds it.
const PROPOSAL_TTL_GRACE: u32 = 100;

#[contract]
pub struct Governance;

#[contractimpl]
impl Governance {
    /// Register the initial member set. `threshold_bps` is the quorum
    /// (basis points of total weight) that "yes" votes must clear for a
    /// proposal to execute. `voting_period_ledgers` bounds how long every
    /// proposal's voting window stays open.
    ///
    /// Membership is fixed at construction: there is no `add_member`. A
    /// governance body for an admin role is expected to be set up once by
    /// its signers, not churned; changing membership means deploying a new
    /// instance and re-pointing the governed contract's admin.
    pub fn __constructor(
        env: Env,
        members: Vec<Address>,
        weights: Vec<u64>,
        threshold_bps: u32,
        voting_period_ledgers: u32,
    ) -> Result<(), Error> {
        if members.len() != weights.len() {
            return Err(Error::ArityMismatch);
        }
        if members.is_empty() || members.len() > MAX_MEMBERS {
            return Err(Error::InvalidMembers);
        }
        if !(MIN_THRESHOLD_BPS..=MAX_THRESHOLD_BPS).contains(&threshold_bps) {
            return Err(Error::InvalidThreshold);
        }
        if voting_period_ledgers == 0 {
            return Err(Error::InvalidVotingPeriod);
        }

        let mut total_weight: u64 = 0;
        for i in 0..members.len() {
            let member = members.get(i).unwrap();
            let weight = weights.get(i).unwrap();
            if weight == 0 {
                return Err(Error::InvalidMembers);
            }
            let key = DataKey::Member(member);
            if env.storage().persistent().has(&key) {
                return Err(Error::InvalidMembers);
            }
            env.storage().persistent().set(&key, &weight);
            total_weight = total_weight
                .checked_add(weight)
                .ok_or(Error::InvalidMembers)?;
        }

        env.storage()
            .instance()
            .set(&DataKey::TotalWeight, &total_weight);
        env.storage()
            .instance()
            .set(&DataKey::ThresholdBps, &threshold_bps);
        env.storage()
            .instance()
            .set(&DataKey::VotingPeriod, &voting_period_ledgers);
        env.storage().instance().set(&DataKey::ProposalCount, &0u64);

        Ok(())
    }

    /// Propose a call to `target::function(args)`. Any member may propose;
    /// the voting window opens immediately and runs for
    /// `voting_period_ledgers` ledgers.
    pub fn propose(
        env: Env,
        proposer: Address,
        target: Address,
        function: Symbol,
        args: Vec<Val>,
    ) -> Result<u64, Error> {
        proposer.require_auth();
        Self::member_weight(&env, &proposer)?;

        let id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::ProposalCount)
            .unwrap_or(0);
        let next_id = id + 1;
        env.storage()
            .instance()
            .set(&DataKey::ProposalCount, &next_id);

        let voting_period: u32 = env
            .storage()
            .instance()
            .get(&DataKey::VotingPeriod)
            .unwrap();
        let deadline_ledger = env.ledger().sequence() + voting_period;

        let proposal = Proposal {
            proposer: proposer.clone(),
            target: target.clone(),
            function: function.clone(),
            args,
            yes_weight: 0,
            no_weight: 0,
            deadline_ledger,
            executed: false,
        };
        let key = DataKey::Proposal(next_id);
        env.storage().persistent().set(&key, &proposal);
        env.storage().persistent().extend_ttl(
            &key,
            voting_period,
            voting_period + PROPOSAL_TTL_GRACE,
        );

        ProposalCreated {
            proposal_id: next_id,
            proposer,
            target,
            function,
            deadline_ledger,
        }
        .publish(&env);

        Ok(next_id)
    }

    /// Cast a weighted vote on an open proposal. Each member may vote once
    /// per proposal.
    pub fn vote(env: Env, voter: Address, proposal_id: u64, support: bool) -> Result<(), Error> {
        voter.require_auth();
        let weight = Self::member_weight(&env, &voter)?;

        let key = DataKey::Proposal(proposal_id);
        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::ProposalNotFound)?;
        if proposal.executed {
            return Err(Error::AlreadyExecuted);
        }
        let now = env.ledger().sequence();
        if now > proposal.deadline_ledger {
            return Err(Error::VotingClosed);
        }

        let voted_key = DataKey::Voted(proposal_id, voter.clone());
        if env.storage().temporary().has(&voted_key) {
            return Err(Error::AlreadyVoted);
        }
        let remaining_ttl = proposal.deadline_ledger.saturating_sub(now);
        env.storage().temporary().set(&voted_key, &());
        env.storage()
            .temporary()
            .extend_ttl(&voted_key, remaining_ttl, remaining_ttl);

        if support {
            proposal.yes_weight += weight;
        } else {
            proposal.no_weight += weight;
        }
        env.storage().persistent().set(&key, &proposal);

        VoteCast {
            proposal_id,
            voter,
            support,
            weight,
        }
        .publish(&env);

        Ok(())
    }

    /// Execute a proposal that has cleared quorum. Callable by anyone —
    /// execution carries no authority beyond what the vote already granted,
    /// and the governed contract's own `require_auth()` on this contract's
    /// address is what actually authorizes the effect (see module docs).
    pub fn execute(env: Env, proposal_id: u64) -> Result<(), Error> {
        let key = DataKey::Proposal(proposal_id);
        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::ProposalNotFound)?;
        if proposal.executed {
            return Err(Error::AlreadyExecuted);
        }

        let total_weight: u64 = env.storage().instance().get(&DataKey::TotalWeight).unwrap();
        let threshold_bps: u32 = env
            .storage()
            .instance()
            .get(&DataKey::ThresholdBps)
            .unwrap();
        let quorum_met = (proposal.yes_weight as u128) * (MAX_THRESHOLD_BPS as u128)
            >= (total_weight as u128) * (threshold_bps as u128);
        if !quorum_met || proposal.yes_weight <= proposal.no_weight {
            return Err(Error::QuorumNotMet);
        }

        // Effects before interaction: persist `executed = true` before the
        // external call, so a reentrant `execute(proposal_id)` triggered
        // from within that call is rejected rather than re-run.
        proposal.executed = true;
        env.storage().persistent().set(&key, &proposal);

        let _: Val =
            env.invoke_contract(&proposal.target, &proposal.function, proposal.args.clone());

        ProposalExecutedEvent {
            proposal_id,
            target: proposal.target,
            function: proposal.function,
        }
        .publish(&env);

        Ok(())
    }

    /// Reclaim a resolved proposal's storage. Callable by anyone once the
    /// proposal is executed, or its voting window has closed without
    /// quorum — never while still active.
    pub fn prune_proposal(env: Env, proposal_id: u64) -> Result<(), Error> {
        let key = DataKey::Proposal(proposal_id);
        let proposal: Proposal = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::ProposalNotFound)?;
        if !proposal.executed && env.ledger().sequence() <= proposal.deadline_ledger {
            return Err(Error::ProposalActive);
        }
        env.storage().persistent().remove(&key);
        Ok(())
    }

    /// Read-only: fetch a proposal's calldata and current tally.
    pub fn get_proposal(env: Env, proposal_id: u64) -> Result<Proposal, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::Proposal(proposal_id))
            .ok_or(Error::ProposalNotFound)
    }

    /// Read-only: a member's weight, or `0` if not a member.
    pub fn get_member_weight(env: Env, member: Address) -> u64 {
        env.storage()
            .persistent()
            .get(&DataKey::Member(member))
            .unwrap_or(0)
    }

    /// Read-only: whether `member` is registered.
    pub fn is_member(env: Env, member: Address) -> bool {
        env.storage().persistent().has(&DataKey::Member(member))
    }

    /// Read-only: whether `voter` has already voted on `proposal_id`.
    pub fn has_voted(env: Env, proposal_id: u64, voter: Address) -> bool {
        env.storage()
            .temporary()
            .has(&DataKey::Voted(proposal_id, voter))
    }

    /// Read-only: sum of every member's weight.
    pub fn get_total_weight(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::TotalWeight)
            .unwrap_or(0)
    }

    /// Read-only: configured quorum, in basis points of total weight.
    pub fn get_threshold_bps(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::ThresholdBps)
            .unwrap_or(0)
    }

    /// Read-only: configured voting window length, in ledgers.
    pub fn get_voting_period(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::VotingPeriod)
            .unwrap_or(0)
    }

    /// Read-only: number of proposals ever created.
    pub fn get_proposal_count(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::ProposalCount)
            .unwrap_or(0)
    }

    fn member_weight(env: &Env, member: &Address) -> Result<u64, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::Member(member.clone()))
            .ok_or(Error::NotAMember)
    }
}
