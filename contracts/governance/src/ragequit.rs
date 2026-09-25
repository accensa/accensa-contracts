//! Ragequit for dissenting governance members (issue #411).
//!
//! Weighted voting alone cannot protect a minority: once a proposal clears
//! quorum it will be executed, even if a member voted against it and
//! disagrees with what their own staked deposit is being used to authorize.
//! [`process`] gives every member who cast a **no** vote on an approved
//! proposal a bounded exit: up to [`RAGEQUIT_WINDOW`] ledgers after the
//! voting deadline they may redeem a pro-rata share of the treasury and have
//! their deposit burned/quarantined — before the proposal executes.
//!
//! # Rules
//!
//! - **Eligibility**: the member voted `support = false` on this proposal
//!   (a dissent marker in temporary storage, set at vote time).
//! - **Approval**: the proposal is currently approved — same quorum-decay
//!   math as [`Governance::execute`](crate::Governance::execute), plus a
//!   strict `yes > no` majority. Unapproved proposals never open a ragequit.
//! - **Window**: from approval until `deadline_ledger + RAGEQUIT_WINDOW`.
//! - **Payout**: `treasury_balance * deposit / total_deposits` (`u128`
//!   checked math). The deposit is removed, the weight leaves
//!   `TotalWeight`, and the redeemed deposit is recorded under
//!   [`DataKey::Quarantined`] — burned for voting-power purposes.
//! - **Once**: a member's deposit can back at most one ragequit; a second
//!   attempt (on any proposal) fails because the deposit is already gone.
//! - **Ordering**: all storage effects are committed before the treasury
//!   token transfer (checks-effects-interactions).

use soroban_sdk::{token, Address, Env};

use crate::quorum::current_quorum_bps;
use crate::voting::quadratic_weight;
use crate::{DataKey, Error, Proposal, MAX_THRESHOLD_BPS, PROPOSAL_TTL_GRACE};

/// Length of the ragequit window, in ledgers: 7 days at ~5 s/ledger.
/// `60 * 60 * 24 * 7 / 5 = 120_960`. Measured from the proposal's voting
/// deadline; an approved proposal opens ragequit immediately, so the
/// effective window is "from approval until `deadline + RAGEQUIT_WINDOW`".
pub const RAGEQUIT_WINDOW: u32 = 120_960;

/// Whether `proposal` is currently approved: quorum (with decay) met and
/// `yes` strictly outweighs `no`. Mirrors the math in
/// [`crate::Governance::execute`] so `execute` and `ragequit` can never
/// disagree about the same proposal's state.
pub(crate) fn is_approved(env: &Env, proposal: &Proposal) -> bool {
    let total_weight: u64 = env
        .storage()
        .instance()
        .get(&DataKey::TotalWeight)
        .unwrap_or(0);
    if total_weight == 0 || proposal.yes_weight <= proposal.no_weight {
        return false;
    }
    let threshold_bps: u32 = env
        .storage()
        .instance()
        .get(&DataKey::ThresholdBps)
        .unwrap_or(0);
    let voting_period: u32 = env
        .storage()
        .instance()
        .get(&DataKey::VotingPeriod)
        .unwrap_or(0);
    let created_ledger = proposal.deadline_ledger.saturating_sub(voting_period);
    let elapsed = env.ledger().sequence().saturating_sub(created_ledger);
    let effective_bps = current_quorum_bps(threshold_bps, elapsed, voting_period);
    (proposal.yes_weight as u128) * (MAX_THRESHOLD_BPS as u128)
        >= (total_weight as u128) * (effective_bps as u128)
}

/// Core ragequit logic behind [`crate::Governance::ragequit`].
pub(crate) fn process(env: &Env, voter: &Address, proposal_id: u64) -> Result<(), Error> {
    let proposal: Proposal = env
        .storage()
        .persistent()
        .get(&DataKey::Proposal(proposal_id))
        .ok_or(Error::ProposalNotFound)?;
    if proposal.executed {
        return Err(Error::AlreadyExecuted);
    }

    // Only members who voted *against* this proposal may ragequit from it.
    if !env
        .storage()
        .temporary()
        .has(&DataKey::Dissent(proposal_id, voter.clone()))
    {
        return Err(Error::NotADissenter);
    }

    // The proposal must actually have cleared quorum + majority.
    if !is_approved(env, &proposal) {
        return Err(Error::ProposalNotApproved);
    }

    // 7-day window after the voting deadline.
    if env.ledger().sequence() > proposal.deadline_ledger.saturating_add(RAGEQUIT_WINDOW) {
        return Err(Error::RagequitWindowClosed);
    }

    if env
        .storage()
        .persistent()
        .has(&DataKey::Ragequit(proposal_id, voter.clone()))
    {
        return Err(Error::AlreadyRagequit);
    }

    let deposit: u64 = env
        .storage()
        .persistent()
        .get(&DataKey::MemberDeposit(voter.clone()))
        .ok_or(Error::NotAMember)?;
    if deposit == 0 {
        return Err(Error::NotAMember);
    }

    let treasury: Address = env
        .storage()
        .instance()
        .get(&DataKey::TreasuryToken)
        .ok_or(Error::TreasuryNotConfigured)?;
    let balance = token::Client::new(env, &treasury).balance(&env.current_contract_address());
    let balance = u128::try_from(balance).map_err(|_| Error::MathOverflow)?;
    let total_deposits: u64 = env
        .storage()
        .instance()
        .get(&DataKey::TotalDeposits)
        .unwrap_or(0);
    // Normal operation always has `total_deposits >= deposit > 0`; the
    // fallback keeps the division defined even against a zeroed sum.
    let denominator = if total_deposits == 0 {
        deposit
    } else {
        total_deposits
    };
    let payout = balance
        .checked_mul(deposit as u128)
        .ok_or(Error::MathOverflow)?
        / denominator as u128;
    let payout = i128::try_from(payout).map_err(|_| Error::MathOverflow)?;

    // Weight the member is giving up — read before the deposit is removed.
    let weight = quadratic_weight(env, voter);

    // ── Effects (before the external transfer) ──────────────────────────
    env.storage()
        .persistent()
        .remove(&DataKey::MemberDeposit(voter.clone()));
    env.storage()
        .persistent()
        .set(&DataKey::Quarantined(voter.clone()), &deposit);
    env.storage()
        .persistent()
        .set(&DataKey::Ragequit(proposal_id, voter.clone()), &());
    env.storage().instance().set(
        &DataKey::TotalDeposits,
        &total_deposits.saturating_sub(deposit),
    );
    let total_weight: u64 = env
        .storage()
        .instance()
        .get(&DataKey::TotalWeight)
        .unwrap_or(0);
    env.storage()
        .instance()
        .set(&DataKey::TotalWeight, &total_weight.saturating_sub(weight));

    // Keep the duplicate guard and quarantine record out of archival range
    // for at least the rest of the ragequit window.
    let marker_ttl = RAGEQUIT_WINDOW.saturating_add(PROPOSAL_TTL_GRACE);
    env.storage().persistent().extend_ttl(
        &DataKey::Quarantined(voter.clone()),
        marker_ttl,
        marker_ttl,
    );
    env.storage().persistent().extend_ttl(
        &DataKey::Ragequit(proposal_id, voter.clone()),
        marker_ttl,
        marker_ttl,
    );

    crate::RagequitEvent {
        proposal_id,
        voter: voter.clone(),
        deposit_burned: deposit,
        payout,
    }
    .publish(env);

    // ── Interaction ─────────────────────────────────────────────────────
    if payout > 0 {
        token::Client::new(env, &treasury).transfer(
            &env.current_contract_address(),
            voter,
            &payout,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;

    use crate::{Error, Governance, GovernanceClient};
    use soroban_sdk::{
        contract, contractimpl,
        testutils::{Address as _, Ledger},
        token::StellarAssetClient,
        Address, Env, IntoVal, Symbol, Vec,
    };

    /// Minimal governed target so proposals have something to execute.
    #[contract]
    struct Target;

    #[contractimpl]
    impl Target {
        pub fn noop(_env: Env) {}
    }

    /// Governed treasury: `set_treasury_token` authorizes via the governance
    /// address (the caller inside `Governance::execute`), mirroring how
    /// `ReceiptAnchor` accepts a contract address as merchant admin.
    #[contract]
    struct GovernedTreasury;

    #[contractimpl]
    impl GovernedTreasury {
        pub fn init(env: Env, governance: Address) {
            env.storage()
                .instance()
                .set(&Symbol::new(&env, "governance"), &governance);
        }

        pub fn set_treasury_token(env: Env, token: Address) {
            let governance: Address = env
                .storage()
                .instance()
                .get(&Symbol::new(&env, "governance"))
                .unwrap();
            governance.require_auth();
            env.storage()
                .instance()
                .set(&Symbol::new(&env, "treasury_token"), &token);
        }

        pub fn get_treasury_token(env: Env) -> Address {
            env.storage()
                .instance()
                .get(&Symbol::new(&env, "treasury_token"))
                .unwrap()
        }
    }

    struct Harness {
        env: Env,
        gov: GovernanceClient<'static>,
        gov_id: Address,
        target: Address,
        token: Address,
        m1: Address,
        m2: Address,
        m3: Address,
    }

    /// Deposits 1/1/4 (weights 1/1/2), 60% quorum, 100-ledger window, a
    /// treasury holding 600 tokens (`total_deposits = 6`).
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

        let sac = env.register_stellar_asset_contract_v2(Address::generate(&env));
        let token = sac.address();
        StellarAssetClient::new(&env, &token).mint(&gov_id, &600i128);
        gov.set_treasury_token(&token);

        let target_id = env.register(Target, ());
        let _ = TargetClient::new(&env, &target_id);

        Harness {
            env,
            gov,
            gov_id,
            target: target_id,
            token,
            m1,
            m2,
            m3,
        }
    }

    /// Propose `Target::noop` and drive it to approval: m3 (2) + m1 (1) yes
    /// = 3 of 4 (75% ≥ 60%), m2 (1) no — m2 is the dissenter.
    fn approved_with_dissent(h: &Harness) -> u64 {
        let args: Vec<soroban_sdk::Val> = Vec::from_array(&h.env, []);
        let id = h
            .gov
            .propose(&h.m1, &h.target, &Symbol::new(&h.env, "noop"), &args);
        h.gov.vote(&h.m3, &id, &true);
        h.gov.vote(&h.m1, &id, &true);
        h.gov.vote(&h.m2, &id, &false);
        id
    }

    #[test]
    fn ragequit_pays_pro_rata_share_and_burns_deposit() {
        let h = setup();
        assert_eq!(h.gov.get_total_deposits(), 6);
        let id = approved_with_dissent(&h);

        h.gov.ragequit(&h.m2, &id);

        // Pro-rata: 600 * 1 / 6 = 100.
        let tc = soroban_sdk::token::TokenClient::new(&h.env, &h.token);
        assert_eq!(tc.balance(&h.m2), 100);
        assert_eq!(tc.balance(&h.gov_id), 500);

        // Deposit burned/quarantined, membership gone, totals updated.
        assert_eq!(h.gov.get_member_deposit(&h.m2), 0);
        assert!(!h.gov.is_member(&h.m2));
        assert_eq!(h.gov.get_quarantined(&h.m2), 1);
        assert_eq!(h.gov.get_total_deposits(), 5);
        assert_eq!(h.gov.get_total_weight(), 3);
        assert!(h.gov.has_ragequit(&id, &h.m2));
        assert!(h.gov.has_dissented(&id, &h.m2));
    }

    #[test]
    fn ragequit_rejects_support_voter() {
        let h = setup();
        let id = approved_with_dissent(&h);
        // m1 voted *yes* — not a dissenter on this proposal.
        assert_eq!(
            h.gov.try_ragequit(&h.m1, &id),
            Err(Ok(Error::NotADissenter))
        );
    }

    #[test]
    fn ragequit_rejects_duplicate_invocation() {
        let h = setup();
        let id = approved_with_dissent(&h);
        h.gov.ragequit(&h.m2, &id);
        assert_eq!(
            h.gov.try_ragequit(&h.m2, &id),
            Err(Ok(Error::AlreadyRagequit))
        );
    }

    #[test]
    fn ragequit_rejects_unapproved_proposal() {
        let h = setup();
        let args: Vec<soroban_sdk::Val> = Vec::from_array(&h.env, []);
        let id = h
            .gov
            .propose(&h.m1, &h.target, &Symbol::new(&h.env, "noop"), &args);
        // m3 (2) yes = 50% < 60% quorum; m2 dissents anyway.
        h.gov.vote(&h.m3, &id, &true);
        h.gov.vote(&h.m2, &id, &false);

        assert_eq!(
            h.gov.try_ragequit(&h.m2, &id),
            Err(Ok(Error::ProposalNotApproved))
        );
        assert_eq!(h.gov.get_member_deposit(&h.m2), 1, "no payout happened");
    }

    #[test]
    fn ragequit_window_closes_after_seven_days_past_deadline() {
        let h = setup();
        let id = approved_with_dissent(&h);

        // Past deadline + RAGEQUIT_WINDOW: the exit path is gone.
        let deadline = h.gov.get_proposal(&id).deadline_ledger;
        h.env
            .ledger()
            .with_mut(|l| l.sequence_number = deadline + crate::ragequit::RAGEQUIT_WINDOW + 1);

        assert_eq!(
            h.gov.try_ragequit(&h.m2, &id),
            Err(Ok(Error::RagequitWindowClosed))
        );
        assert_eq!(h.gov.get_member_deposit(&h.m2), 1);
    }

    #[test]
    fn ragequit_succeeds_inside_window_after_deadline() {
        let h = setup();
        let id = approved_with_dissent(&h);

        let deadline = h.gov.get_proposal(&id).deadline_ledger;
        h.env
            .ledger()
            .with_mut(|l| l.sequence_number = deadline + 1);
        h.gov.ragequit(&h.m2, &id);
        assert!(h.gov.has_ragequit(&id, &h.m2));
    }

    #[test]
    fn ragequit_rejects_when_treasury_not_configured() {
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
        let _ = TargetClient::new(&env, &target_id);

        let args: Vec<soroban_sdk::Val> = Vec::from_array(&env, []);
        let id = gov.propose(&m1, &target_id, &Symbol::new(&env, "noop"), &args);
        gov.vote(&m3, &id, &true);
        gov.vote(&m1, &id, &true);
        gov.vote(&m2, &id, &false);

        assert_eq!(
            gov.try_ragequit(&m2, &id),
            Err(Ok(Error::TreasuryNotConfigured))
        );
    }

    #[test]
    fn set_treasury_token_runs_through_governance_execute() {
        let env = Env::default();
        env.mock_all_auths();
        let m1 = Address::generate(&env);
        let members = Vec::from_array(&env, [m1.clone()]);
        let deposits = Vec::from_array(&env, [4u64]);
        // 100% quorum, single member: propose + vote + execute.
        let gov_id = env.register(Governance, (members, deposits, 10_000u32, 100u32));
        let gov = GovernanceClient::new(&env, &gov_id);

        let treasury_id = env.register(GovernedTreasury, ());
        let treasury = GovernedTreasuryClient::new(&env, &treasury_id);
        treasury.init(&gov_id);

        let sac = env.register_stellar_asset_contract_v2(Address::generate(&env));
        let token = sac.address();

        // `execute` calls the governed contract, whose `set_treasury_token`
        // requires authentication from the governance address (the direct
        // caller) — the same convention `ReceiptAnchor` relies on.
        let args = Vec::from_array(&env, [token.into_val(&env)]);
        let id = gov.propose(
            &m1,
            &treasury_id,
            &Symbol::new(&env, "set_treasury_token"),
            &args,
        );
        gov.vote(&m1, &id, &true);
        gov.execute(&id);

        assert_eq!(treasury.get_treasury_token(), token);
    }

    #[test]
    fn second_proposal_ragequit_blocked_after_deposit_already_redeemed() {
        let h = setup();
        let args: Vec<soroban_sdk::Val> = Vec::from_array(&h.env, []);
        let sym = Symbol::new(&h.env, "noop");

        // Both proposals get a dissent vote from m2 *before* any ragequit.
        let id1 = h.gov.propose(&h.m1, &h.target, &sym, &args);
        h.gov.vote(&h.m3, &id1, &true);
        h.gov.vote(&h.m1, &id1, &true);
        h.gov.vote(&h.m2, &id1, &false);

        let id2 = h.gov.propose(&h.m1, &h.target, &sym, &args);
        h.gov.vote(&h.m3, &id2, &true);
        h.gov.vote(&h.m1, &id2, &true);
        h.gov.vote(&h.m2, &id2, &false);

        h.gov.ragequit(&h.m2, &id1);
        // Deposit is gone: a second ragequit — even against another
        // approved proposal with a dissent vote — cannot pay twice.
        assert_eq!(h.gov.try_ragequit(&h.m2, &id2), Err(Ok(Error::NotAMember)));
        assert_eq!(h.gov.get_member_deposit(&h.m2), 0);
    }

    #[test]
    fn prune_rejects_approved_proposal_through_ragequit_window() {
        let h = setup();
        let id = approved_with_dissent(&h);

        // Voting window over, but the proposal is approved and still inside
        // the ragequit window — pruning would strand dissenters.
        let deadline = h.gov.get_proposal(&id).deadline_ledger;
        h.env
            .ledger()
            .with_mut(|l| l.sequence_number = deadline + 1);
        assert_eq!(
            h.gov.try_prune_proposal(&id),
            Err(Ok(Error::ProposalActive))
        );

        // Once the ragequit window lapses, anyone may reclaim the rent.
        h.env
            .ledger()
            .with_mut(|l| l.sequence_number = deadline + crate::ragequit::RAGEQUIT_WINDOW + 1);
        h.gov.prune_proposal(&id);
        assert_eq!(
            h.gov.try_get_proposal(&id),
            Err(Ok(Error::ProposalNotFound))
        );
    }

    #[test]
    fn ragequit_rejects_executed_proposal() {
        let h = setup();
        let id = approved_with_dissent(&h);
        h.gov.execute(&id);
        assert_eq!(
            h.gov.try_ragequit(&h.m2, &id),
            Err(Ok(Error::AlreadyExecuted))
        );
    }

    #[test]
    fn ragequit_rejects_non_member() {
        let h = setup();
        let id = approved_with_dissent(&h);
        let outsider = Address::generate(&h.env);
        assert_eq!(
            h.gov.try_ragequit(&outsider, &id),
            Err(Ok(Error::NotADissenter))
        );
    }
}
