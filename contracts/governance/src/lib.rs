//! A proposal-based, weighted-voting governance contract for Soroban admin
//! roles (issue: single-admin-key SPOF on `ReceiptAnchor`).
//!
//! ## Liquid Staking
//!
//! Liquid Staking Derivative (stACC) module: mint stACC 1:1 upon locking
//! underlying tokens, burn stACC to redeem after lock epoch, and calculate
//! redemption value via exchange rate / share progression.
//!
//! Primary contract file: `contracts/governance/src/liquid_staking.rs`
//!
//! Re-exported types:
//! - `LiquidStakingError` - contract errors
//! - `LiquidStakingDataKey` - storage data keys
//! - `ExchangeRate` - 1e6 precision stACC/underlying rate
//! - `UserData` - per-user balance/lock state
//! - `Mint` - mint event
//! - `Burn` - burn event
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
//! - A member's vote *against* a proposal also drops a `Dissent(id, Address)`
//!   marker in temporary storage (issue #411): it is what makes
//!   [`Governance::ragequit`] callable for that member, and it expires with
//!   the proposal's ragequit window. `Quarantined(Address)` records deposits
//!   already redeemed through ragequit, `TotalDeposits` / `TreasuryToken`
//!   carry the pro-rata base and the SEP-41 payout token, and
//!   `Ragequit(id, Address)` is the persistent duplicate guard.

#![no_std]

#[cfg(test)]
extern crate alloc;

#[cfg(test)]
mod test;

mod liquid_staking;
mod math;
mod quorum;
mod ragequit;
mod ring_sig;
pub mod simulation;
mod voting;

pub use liquid_staking::{
    Burn, ExchangeRate, LiquidStaking, LiquidStakingClient, LiquidStakingDataKey,
    LiquidStakingError, Mint, UserData, UserData as LiquidStakingUserData,
};

pub mod optimistic;

use quorum::current_quorum_bps;

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contractmeta, contracttype, Address,
    BytesN, Env, Symbol, Val, Vec,
};

use voting::{quadratic_weight, register_deposit};

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
    /// The proposal has not cleared quorum and majority, so no ragequit
    /// window is open for it (issue #411).
    ProposalNotApproved = 12,
    /// The 7-day ragequit window after the voting deadline has closed
    /// (issue #411).
    RagequitWindowClosed = 13,
    /// This member already ragequit against this proposal (issue #411).
    AlreadyRagequit = 14,
    /// The caller did not vote against this proposal, so cannot ragequit
    /// from it (issue #411).
    NotADissenter = 15,
    /// No treasury token has been configured for ragequit payouts
    /// (issue #411).
    TreasuryNotConfigured = 16,
    /// A checked arithmetic operation in the ragequit payout math
    /// over- or under-flowed, or a conversion would truncate (issue #411).
    MathOverflow = 17,
    /// A proposal was submitted without the simulation report the current
    /// configuration requires (issue #483).
    SimulationRequired = 18,
    /// The submitted simulation report failed verification: wrong simulator,
    /// stale binding hash, or not bound to this proposal's calldata
    /// (issue #483).
    SimulationMismatch = 19,
    /// The simulation report's outcome says the proposal would revert
    /// (issue #483).
    SimulationFailed = 20,
    /// Simulation is required but no simulator contract is registered
    /// (issue #483).
    SimulationNotConfigured = 21,
    /// No optimistic proposal exists with the given id.
    OptimisticNotFound = 22,
    /// The optimistic proposal was vetoed and cannot execute.
    OptimisticVetoed = 23,
    /// The optimistic challenge window has closed.
    ChallengeWindowClosed = 24,
    /// This member already vetoed this optimistic proposal.
    AlreadyVetoed = 25,
    /// A ring or LSAG signature is malformed or does not verify.
    InvalidRingSignature = 26,
    /// An LSAG key image has already voted on this proposal.
    DuplicateKeyImage = 27,
    /// The selected ring is not a valid registered anonymity set.
    InvalidAnonymitySet = 28,
    /// A vote would duplicate an address vote or follow an anonymous-mode lock.
    VotingModeConflict = 29,
    /// The member already registered a voting key or the key is already used.
    VotingKeyAlreadyRegistered = 30,
    /// A registered voting key is not a canonical Ristretto255 point.
    InvalidVotingKey = 31,
}

#[contracttype]
pub enum DataKey {
    /// Instance: sum of every member's quadratic weight.
    TotalWeight,
    /// Instance: quorum, in basis points (`1..=10_000`) of `TotalWeight`.
    ThresholdBps,
    /// Instance: length of a proposal's voting window, in ledgers.
    VotingPeriod,
    /// Instance: number of proposals ever created; also the next id.
    ProposalCount,
    /// Persistent, one entry per member: that member's deposited governance tokens.
    MemberDeposit(Address),
    /// Persistent: a proposal's calldata and running tally.
    Proposal(u64),
    /// Temporary: marks that `.1` already voted on proposal `.0`.
    Voted(u64, Address),
    /// Temporary: marks that `.1` voted *against* proposal `.0` — the
    /// eligibility ticket for `ragequit` (issue #411). Lives until the
    /// proposal's ragequit window closes.
    Dissent(u64, Address),
    /// Persistent: marks that `.1` already ragequit against proposal `.0`
    /// (issue #411); duplicate guard on top of the removed deposit.
    Ragequit(u64, Address),
    /// Persistent: the raw deposit a member redeemed (and thus burned for
    /// voting power) when they ragequit (issue #411).
    Quarantined(Address),
    /// Instance: sum of every member's raw deposited tokens — the pro-rata
    /// denominator for ragequit payouts (issue #411).
    TotalDeposits,
    /// Instance: the SEP-41 token that backs ragequit withdrawals, set via
    /// `set_treasury_token` through an executed proposal (issue #411).
    TreasuryToken,
    /// Persistent: whether proposals must carry a simulation report and
    /// which simulator accepts reports (issue #483).
    SimulationConfig,
    /// Persistent: the verified simulation report stored with a proposal
    /// created through `propose_with_simulation` (issue #483).
    SimAttestation(u64),
    /// Instance: number of optimistic proposals ever created; also the next id.
    OptimisticCount,
    /// Persistent: an optimistic proposal's payload and veto tally.
    OptimisticProposal(u64),
    /// Temporary: marks that `.1` vetoed optimistic proposal `.0`.
    OptimisticVeto(u64, Address),
    /// Persistent: the member's public LSAG voting key.
    MemberVotingKey(Address),
    /// Persistent reverse index for registered LSAG keys.
    VotingKeyMember(BytesN<32>),
    /// Temporary: a proposal has received an anonymous vote and cannot then
    /// accept address-authenticated votes that could double-count a signer.
    AnonymousVoting(u64),
    /// Temporary: linkable LSAG image already used for a proposal.
    KeyImage(u64, BytesN<32>),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RingSignature {
    pub key_image: BytesN<32>,
    pub initial_challenge: BytesN<32>,
    pub responses: Vec<BytesN<32>>,
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

/// Emitted after an anonymous LSAG vote has verified. It contains no signer address or key image.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnonymousVoteCast {
    #[topic]
    pub proposal_id: u64,
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

/// Emitted when a member ragequits out of an approved proposal
/// (issue #411).
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RagequitEvent {
    #[topic]
    pub proposal_id: u64,
    #[topic]
    pub voter: Address,
    /// Raw deposit removed from the member and quarantined (burned).
    pub deposit_burned: u64,
    /// Pro-rata treasury tokens transferred to the member.
    pub payout: i128,
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
        deposits: Vec<u64>,
        threshold_bps: u32,
        voting_period_ledgers: u32,
    ) -> Result<(), Error> {
        if members.len() != deposits.len() {
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
        let mut total_deposits: u64 = 0;
        for i in 0..members.len() {
            let member = members.get(i).unwrap();
            let deposit = deposits.get(i).unwrap();
            if deposit == 0 {
                return Err(Error::InvalidMembers);
            }
            let key = DataKey::MemberDeposit(member.clone());
            if env.storage().persistent().has(&key) {
                return Err(Error::InvalidMembers);
            }
            register_deposit(&env, &member, deposit);
            total_deposits = total_deposits
                .checked_add(deposit)
                .ok_or(Error::InvalidMembers)?;
            total_weight = total_weight
                .checked_add(quadratic_weight(&env, &member))
                .ok_or(Error::InvalidMembers)?;
        }

        env.storage()
            .instance()
            .set(&DataKey::TotalWeight, &total_weight);
        env.storage()
            .instance()
            .set(&DataKey::TotalDeposits, &total_deposits);
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
    ///
    /// When simulation is configured as required (see
    /// [`simulation`]), this path is refused with
    /// [`Error::SimulationRequired`] — use
    /// [`propose_with_simulation`](Self::propose_with_simulation) instead.
    pub fn propose(
        env: Env,
        proposer: Address,
        target: Address,
        function: Symbol,
        args: Vec<Val>,
    ) -> Result<u64, Error> {
        proposer.require_auth();
        Self::member_deposit(&env, &proposer)?;

        // Simulation hook (issue #483): when a proposal-simulation oracle is
        // registered as mandatory, the un-reported path is closed.
        if simulation::is_required(&env) {
            return Err(Error::SimulationRequired);
        }

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
        // Cover the voting window *plus* the ragequit window (issue #411):
        // an approved proposal must stay readable for the whole 7 days in
        // which dissenters may still exit. Threshold == extend_to so a
        // freshly-written entry (already at the network's minimum TTL) is
        // actually extended instead of silently skipped.
        let ttl = voting_period
            .saturating_add(ragequit::RAGEQUIT_WINDOW)
            .saturating_add(PROPOSAL_TTL_GRACE);
        env.storage().persistent().extend_ttl(&key, ttl, ttl);

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
    /// per proposal. Voting power is the integer square root of the
    /// member's deposited governance tokens.
    pub fn vote(env: Env, voter: Address, proposal_id: u64, support: bool) -> Result<(), Error> {
        voter.require_auth();
        if env
            .storage()
            .temporary()
            .has(&DataKey::AnonymousVoting(proposal_id))
            && env
                .storage()
                .persistent()
                .has(&DataKey::MemberVotingKey(voter.clone()))
        {
            return Err(Error::VotingModeConflict);
        }
        let weight = quadratic_weight(&env, &voter);
        if weight == 0 {
            return Err(Error::NotAMember);
        }

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
            proposal.yes_weight = proposal.yes_weight.saturating_add(weight);
        } else {
            proposal.no_weight = proposal.no_weight.saturating_add(weight);
            // Record dissent so this member can ragequit if the proposal is
            // later approved (issue #411). The marker outlives the voting
            // window by the full ragequit window plus grace, so it is still
            // there when `ragequit` is called after the deadline.
            let dissent_key = DataKey::Dissent(proposal_id, voter.clone());
            let dissent_ttl = proposal
                .deadline_ledger
                .saturating_sub(now)
                .saturating_add(ragequit::RAGEQUIT_WINDOW)
                .saturating_add(PROPOSAL_TTL_GRACE);
            env.storage().temporary().set(&dissent_key, &());
            env.storage()
                .temporary()
                .extend_ttl(&dissent_key, dissent_ttl, dissent_ttl);
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

    /// Register a member's Ristretto255 LSAG public key. A member can register
    /// one unique key; the secret key remains off-chain. Authentication proves
    /// authority to bind the key to the member, while later LSAG proofs prove
    /// secret-key possession without identifying the member.
    pub fn register_voting_key(
        env: Env,
        member: Address,
        public_key: BytesN<32>,
    ) -> Result<(), Error> {
        member.require_auth();
        Self::member_deposit(&env, &member)?;
        if env
            .storage()
            .persistent()
            .has(&DataKey::MemberVotingKey(member.clone()))
            || env
                .storage()
                .persistent()
                .has(&DataKey::VotingKeyMember(public_key.clone()))
        {
            return Err(Error::VotingKeyAlreadyRegistered);
        }
        if !ring_sig::valid_public_key(&public_key) {
            return Err(Error::InvalidVotingKey);
        }
        env.storage()
            .persistent()
            .set(&DataKey::MemberVotingKey(member.clone()), &public_key);
        env.storage()
            .persistent()
            .set(&DataKey::VotingKeyMember(public_key), &member);
        Ok(())
    }

    /// Cast a linkable-ring-signature vote without supplying or storing the
    /// signer's address. Every key in `ring` must belong to a registered
    /// member and all ring members must have equal quadratic weight. This
    /// preserves exact tally weights while preventing the tally from revealing
    /// which member in the ring signed. A member who has already voted
    /// transparently cannot be in this ring; after an anonymous vote, the
    /// proposal no longer accepts transparent votes. Anonymous dissent does
    /// not create an address-keyed ragequit marker.
    pub fn vote_anonymous(
        env: Env,
        proposal_id: u64,
        support: bool,
        ring: Vec<BytesN<32>>,
        signature: RingSignature,
    ) -> Result<(), Error> {
        if ring.len() < 2 || ring.len() > MAX_MEMBERS {
            return Err(Error::InvalidAnonymitySet);
        }
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
        let mut weight: Option<u64> = None;
        for i in 0..ring.len() {
            let public_key = ring.get(i).unwrap();
            let Some(member) = env
                .storage()
                .persistent()
                .get::<_, Address>(&DataKey::VotingKeyMember(public_key.clone()))
            else {
                return Err(Error::InvalidAnonymitySet);
            };
            // Do not let a previously transparent voter hide behind an
            // anonymity ring that includes their registered key.
            if env
                .storage()
                .temporary()
                .has(&DataKey::Voted(proposal_id, member.clone()))
            {
                return Err(Error::VotingModeConflict);
            }
            let member_weight = quadratic_weight(&env, &member);
            if member_weight == 0
                || weight.is_some_and(|w| w != member_weight)
                || (0..i).any(|j| ring.get(j).as_ref() == Some(&public_key))
            {
                return Err(Error::InvalidAnonymitySet);
            }
            weight = Some(member_weight);
        }
        let weight = weight.ok_or(Error::InvalidAnonymitySet)?;
        let message = ring_sig::vote_message(&env, proposal_id, support, &ring);
        if !ring_sig::verify(&env, proposal_id, &ring, &message, &signature) {
            return Err(Error::InvalidRingSignature);
        }
        let image_key = DataKey::KeyImage(proposal_id, signature.key_image.clone());
        if env.storage().temporary().has(&image_key) {
            return Err(Error::DuplicateKeyImage);
        }

        // All validation is complete; mutations begin here.
        let remaining_ttl = proposal.deadline_ledger.saturating_sub(now);
        env.storage()
            .temporary()
            .set(&DataKey::AnonymousVoting(proposal_id), &());
        env.storage().temporary().extend_ttl(
            &DataKey::AnonymousVoting(proposal_id),
            remaining_ttl,
            remaining_ttl,
        );
        env.storage().temporary().set(&image_key, &());
        env.storage()
            .temporary()
            .extend_ttl(&image_key, remaining_ttl, remaining_ttl);
        if support {
            proposal.yes_weight = proposal.yes_weight.saturating_add(weight);
        } else {
            proposal.no_weight = proposal.no_weight.saturating_add(weight);
        }
        env.storage().persistent().set(&key, &proposal);
        AnonymousVoteCast {
            proposal_id,
            support,
            weight,
        }
        .publish(&env);
        Ok(())
    }

    /// Return the exact domain-separated bytes that `vote_anonymous` verifies.
    /// Off-chain signers should sign this value rather than reimplementing its
    /// XDR and network encoding.
    pub fn get_anonymous_vote_message(
        env: Env,
        proposal_id: u64,
        support: bool,
        ring: Vec<BytesN<32>>,
    ) -> soroban_sdk::Bytes {
        ring_sig::vote_message(&env, proposal_id, support, &ring)
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
        let voting_period: u32 = env
            .storage()
            .instance()
            .get(&DataKey::VotingPeriod)
            .unwrap();

        // A proposal's window opened the ledger it was created on; the
        // effective quorum decays linearly from the initial threshold toward
        // the safety floor as that window elapses (see [`quorum`]).
        let created_ledger = proposal.deadline_ledger.saturating_sub(voting_period);
        let now = env.ledger().sequence();
        let elapsed = now.saturating_sub(created_ledger);
        let effective_bps = current_quorum_bps(threshold_bps, elapsed, voting_period);

        let quorum_met = (proposal.yes_weight as u128) * (MAX_THRESHOLD_BPS as u128)
            >= (total_weight as u128) * (effective_bps as u128);
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
        if !proposal.executed {
            let now = env.ledger().sequence();
            if now <= proposal.deadline_ledger {
                return Err(Error::ProposalActive);
            }
            // An *approved* proposal must stay readable through its entire
            // ragequit window (issue #411) — pruning it early would strand
            // dissenters who have not exited yet. Unapproved proposals remain
            // prunable the moment their voting window closes.
            if ragequit::is_approved(&env, &proposal)
                && now
                    <= proposal
                        .deadline_ledger
                        .saturating_add(ragequit::RAGEQUIT_WINDOW)
            {
                return Err(Error::ProposalActive);
            }
        }
        env.storage().persistent().remove(&key);
        Ok(())
    }

    /// Configure the SEP-41 token that backs ragequit withdrawals
    /// (issue #411).
    ///
    /// Gated by `current_contract_address().require_auth()`: the host
    /// satisfies that only when *this* contract is the direct caller of the
    /// invocation — i.e. when the call arrives through
    /// [`execute`](Self::execute) as an approved proposal targeting this
    /// contract. A direct external call fails authorization, so the treasury
    /// can only ever change through a vote.
    pub fn set_treasury_token(env: Env, token: Address) -> Result<(), Error> {
        env.current_contract_address().require_auth();
        env.storage()
            .instance()
            .set(&DataKey::TreasuryToken, &token);
        Ok(())
    }

    /// Ragequit out of an approved proposal (issue #411).
    ///
    /// `voter_auth` must have voted *against* `proposal_id`. Callable from
    /// approval until `deadline_ledger + RAGEQUIT_WINDOW` (7 days), after
    /// which the member's only remaining path is to live with the outcome.
    /// Pays `treasury_balance * deposit / total_deposits`, removes the
    /// member's deposit (quarantined — burned for voting power), shrinks
    /// `TotalWeight` / `TotalDeposits`, and can never be repeated for this
    /// member against any proposal.
    pub fn ragequit(env: Env, voter_auth: Address, proposal_id: u64) -> Result<(), Error> {
        voter_auth.require_auth();
        ragequit::process(&env, &voter_auth, proposal_id)
    }

    // ── Proposal simulation hooks (issue #483) ──────────────────────────

    /// Propose a call with a verified simulation report. The report must
    /// come from the registered simulator, be bound to this exact proposal
    /// id and calldata (`sim_hash`), and report a non-reverting dry-run
    /// (`outcome == 0`). When simulation is configured as required, this is
    /// the only accepted proposal path; the report is stored alongside the
    /// proposal so voters can inspect it. See [`simulation`].
    pub fn propose_with_simulation(
        env: Env,
        proposer: Address,
        target: Address,
        function: Symbol,
        args: Vec<Val>,
        report: simulation::SimulationReport,
    ) -> Result<u64, Error> {
        proposer.require_auth();
        simulation::propose_with_simulation(&env, &proposer, target, function, args, report)
    }

    /// Configure proposal simulation (member auth): which simulator contract
    /// accepts reports, and whether every new proposal must carry a
    /// successful one. Pass `None` to unregister the simulator (only
    /// allowed while `required` is false).
    pub fn set_simulation_config(
        env: Env,
        member: Address,
        simulator: Option<Address>,
        required: bool,
    ) -> Result<(), Error> {
        member.require_auth();
        Self::member_deposit(&env, &member)?;
        simulation::set_config(&env, simulator, required)
    }

    /// Read-only: the current simulation configuration, if any.
    pub fn get_simulation_config(env: Env) -> Option<simulation::SimulationConfig> {
        simulation::config(&env)
    }

    /// Read-only: the simulation report stored for `proposal_id`, if the
    /// proposal was created through [`propose_with_simulation`].
    pub fn get_simulation_report(
        env: Env,
        proposal_id: u64,
    ) -> Option<simulation::SimulationReport> {
        simulation::get_report(&env, proposal_id)
    }

    /// Read-only: fetch a proposal's calldata and current tally.
    pub fn get_proposal(env: Env, proposal_id: u64) -> Result<Proposal, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::Proposal(proposal_id))
            .ok_or(Error::ProposalNotFound)
    }

    /// Read-only: a member's quadratic weight, or `0` if not a member.
    pub fn get_member_weight(env: Env, member: Address) -> u64 {
        quadratic_weight(&env, &member)
    }

    /// Read-only: whether `member` is registered.
    pub fn is_member(env: Env, member: Address) -> bool {
        env.storage()
            .persistent()
            .has(&DataKey::MemberDeposit(member))
    }

    /// Read-only: whether `voter` has already voted on `proposal_id`.
    pub fn has_voted(env: Env, proposal_id: u64, voter: Address) -> bool {
        env.storage()
            .temporary()
            .has(&DataKey::Voted(proposal_id, voter))
    }

    /// Read-only: a member's raw deposit, or `0` if not a member.
    pub fn get_member_deposit(env: Env, member: Address) -> u64 {
        env.storage()
            .persistent()
            .get(&DataKey::MemberDeposit(member))
            .unwrap_or(0)
    }

    /// Read-only: sum of every member's quadratic weight.
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

    /// Read-only: sum of every member's raw deposited tokens — ragequit's
    /// pro-rata denominator (issue #411).
    pub fn get_total_deposits(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::TotalDeposits)
            .unwrap_or(0)
    }

    /// Read-only: the SEP-41 treasury token configured for ragequit, if any
    /// (issue #411).
    pub fn get_treasury_token(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::TreasuryToken)
    }

    /// Read-only: the deposit a member redeemed (burned) via ragequit, or
    /// `0` if they never ragequit (issue #411).
    pub fn get_quarantined(env: Env, member: Address) -> u64 {
        env.storage()
            .persistent()
            .get(&DataKey::Quarantined(member))
            .unwrap_or(0)
    }

    /// Read-only: whether `member` already ragequit against `proposal_id`
    /// (issue #411).
    pub fn has_ragequit(env: Env, proposal_id: u64, member: Address) -> bool {
        env.storage()
            .persistent()
            .has(&DataKey::Ragequit(proposal_id, member))
    }

    /// Read-only: whether `voter` cast a dissenting (no) vote on
    /// `proposal_id` whose marker is still live (issue #411).
    pub fn has_dissented(env: Env, proposal_id: u64, voter: Address) -> bool {
        env.storage()
            .temporary()
            .has(&DataKey::Dissent(proposal_id, voter))
    }

    /// Queue a routine operation for optimistic execution (issue #475).
    /// `proposer`, a member, authorizes the queue; the proposal becomes
    /// executable immediately and opens a 24-hour supermajority-veto window.
    /// Returns the new optimistic proposal id.
    pub fn optimistic_submit(
        env: Env,
        proposer: Address,
        target: Address,
        function: Symbol,
        args: Vec<Val>,
    ) -> Result<u64, Error> {
        optimistic::submit(&env, &proposer, &target, &function, &args)
    }

    /// Cast `voter`'s weight against optimistic proposal `proposal_id`
    /// (issue #475). Only valid inside the challenge window, only members,
    /// once per member. When cumulative veto weight reaches a ~2/3
    /// supermajority of total weight the proposal is locked and execution
    /// reverts with [`Error::OptimisticVetoed`].
    pub fn veto_optimistic(env: Env, voter: Address, proposal_id: u64) -> Result<(), Error> {
        optimistic::veto(&env, &voter, proposal_id)
    }

    /// Execute queued optimistic proposal `proposal_id` against its target
    /// (issue #475). Anyone may call, at any time, unless the proposal was
    /// executed already or a supermajority vetoed it during the window.
    pub fn execute_optimistic(env: Env, proposal_id: u64) -> Result<(), Error> {
        optimistic::execute(&env, proposal_id)
    }

    /// Read-only: the current state of optimistic proposal `proposal_id`
    /// (issue #475).
    pub fn get_optimistic_proposal(
        env: Env,
        proposal_id: u64,
    ) -> Result<optimistic::OptimisticProposal, Error> {
        optimistic::get(&env, proposal_id)
    }

    fn member_deposit(env: &Env, member: &Address) -> Result<(), Error> {
        env.storage()
            .persistent()
            .get::<_, u64>(&DataKey::MemberDeposit(member.clone()))
            .ok_or(Error::NotAMember)?;
        Ok(())
    }
}
// audit implementation

/// Graceful downgrade strategy parameters for protocol upgrades.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DowngradeStrategy {
    pub active_protocol_version: u32,
    pub fallback_protocol_version: u32,
    pub emergency_mode_enabled: bool,
}
