//! Per-bounty community donations and quadratic treasury matching
//! (issue #476).
//!
//! # What exists, and what this module adds
//!
//! Before this module the governance contract had **no bounty entity, no
//! bounty identifier type and no bounty-completion hook**: the only "bounty"
//! features in the workspace are `state-channel`'s per-channel watchtower
//! reward (issue #459) and `receipt-shard`'s per-batch pruning bounty, both
//! unrelated to DAO funding, and neither lives in this contract. This module
//! therefore owns a deliberately minimal bounty registry keyed by `u64` —
//! the identifier width both existing bounty features use — and adds no new
//! authority: opening and completing a bounty are gated by
//! `current_contract_address().require_auth()`, exactly like
//! `set_treasury_token` (issue #411) and `enable_timelock` (issue #447), so
//! both are reachable only through an approved proposal executed against
//! this contract.
//!
//! - [`Governance::qf_open_bounty`] records a bounty id and its recipient.
//! - [`Governance::qf_donate`] takes `amount` of the configured SEP-41
//!   `TreasuryToken` into this contract's custody and records the donor's
//!   **cumulative** contribution to that bounty.
//! - [`Governance::qf_complete_bounty`] pays the recipient the accumulated
//!   donations plus the treasury match **exactly once** and closes the
//!   bounty to further donations.
//!
//! # The quadratic match, and its rounding rule
//!
//! The theoretical match of a bounty is
//!
//! ```text
//! match = (Σᵢ √cᵢ)² − Σᵢ cᵢ
//! ```
//!
//! over every donor `i`'s cumulative contribution `cᵢ` — donor-level, never
//! total-level, so the same total donated by a crowd always matches more
//! than the same total donated by one account.
//!
//! Every square root is computed in **fixed point** at [`QF_SCALE`] =
//! 10⁷ (one seven-decimal asset unit of precision, matching the 7-decimal
//! Stellar assets this workspace uses; see the README's amount semantics).
//! The per-donor term is `tᵢ = ⌊√(cᵢ · 10¹⁴)⌋`, and each bounty keeps the
//! running sum `T = Σ tᵢ` **up to date incrementally** at donation time, so
//! completion is O(1) and never iterates over donors (Soroban contracts
//! cannot enumerate storage keys, and an unbounded per-completion loop would
//! be a CPU-budget hazard). The match is then
//!
//! ```text
//! match = ⌊T² / 10¹⁴⌋ − Σᵢ cᵢ
//! ```
//!
//! in exact integer arithmetic — no floating point anywhere.
//!
//! **Rounding rule: every root and every division floors, and the final
//! result is clamped at zero**, so a match can never exceed the theoretical
//! value — the treasury can only be under-matched. For `n` donors the
//! scaled-root flooring costs at most `2·(Σ√cᵢ)·n·10⁻⁷` base units and the
//! final integer floor at most one more stroop: always in the treasury's
//! favor, never the donor's. Flooring each root *without* the fixed-point
//! scale, by contrast, loses up to a full stroop per donor *before*
//! squaring, which is materially wrong for small token amounts: two
//! 2-stroop donors would match 0 instead of 4 stroops, where this
//! fixed-point scheme recovers 3.
//!
//! A single donor's match is always zero (`(⌊√c⌋)² ≤ c` after clamping), so
//! one donor never gains a multi-donor matching advantage, and repeat
//! donations from one `Address` are aggregated into a single cumulative
//! contribution — which removes the *accidental* form of the split-donation
//! gain. A determined attacker who controls many addresses can still
//! amplify the match; this module follows the repository's existing donor
//! identity policy — address authentication, and nothing beyond it — and
//! deliberately invents no verification system. That residual Sybil
//! assumption is called out for maintainers.
//!
//! # Treasury interaction and what is deliberately not invented
//!
//! Donations are held in this contract's SEP-41 `TreasuryToken` balance —
//! the same balance that backs `ragequit` (issue #411); Soroban has no
//! segregated sub-account, so the two features share it. `qf_complete_bounty`
//! pays `total_donated + match` to the recorded recipient: the donated
//! principal merely passes through, and only the match is charged to the
//! treasury. If the treasury balance — net of the donations it holds for all
//! bounties — cannot cover the match, the call fails with
//! [`QfError::InsufficientTreasury`] and leaves every record and balance
//! untouched, the same fail-closed behaviour as `refund-vault`'s
//! `InsufficientFloat` and `treasury`'s `InsufficientBalance`. The existing
//! architecture defines **no** per-bounty matching cap, no global matching
//! budget and no donation cutoff ledger, so this module deliberately
//! invents none; completion (proposal-gated) is the cutoff.
//!
//! # Arithmetic bounds
//!
//! Donation amounts are `i128` base units. A donation (or a donor's
//! cumulative total) must satisfy `amount · 10¹⁴ ≤ u128::MAX`
//! (≈ 3.4 × 10²⁴ base units ≈ 3.4 × 10¹⁷ tokens); beyond that the scaled
//! square root fails to compute and the call returns [`QfError::MathOverflow`]
//! without touching state. `T²` can exceed `u128::MAX` only for extreme
//! multi-donor totals in that range; that too surfaces as a typed
//! [`QfError::MathOverflow`] rather than a trap.

use soroban_sdk::{contracterror, contractevent, contractimpl, contracttype, token, Address, Env};

use crate::{DataKey, Governance, GovernanceArgs, GovernanceClient};

/// Fixed-point scale for the square-root terms: one seven-decimal asset
/// unit of precision.
pub const QF_SCALE: u128 = 10_000_000;

/// `QF_SCALE²` — the divisor that brings `T²` back to base units.
pub const QF_SCALE_SQ: u128 = QF_SCALE * QF_SCALE;

/// Errors of the quadratic-funding functions. Local to this module, like
/// `VeError` (issue #438), so the governance `Error` enum — and with it the
/// spec every deployment carries — does not grow for an optional feature.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum QfError {
    /// No bounty registry entry exists for that id (never opened, or an
    /// id in the distance).
    BountyNotFound = 1,
    /// A bounty with that id already exists; ids are not reusable.
    BountyAlreadyExists = 2,
    /// The bounty was completed; it no longer accepts donations.
    BountyNotOpen = 3,
    /// The bounty was already completed and already received its match —
    /// the duplicate-payout guard.
    BountyAlreadyCompleted = 4,
    /// The donation amount was not strictly positive.
    InvalidAmount = 5,
    /// No `TreasuryToken` has been configured, so donations cannot be
    /// custodied nor matches paid.
    TreasuryNotConfigured = 6,
    /// The treasury balance (net of donations held for bounties) cannot
    /// cover this bounty's match; nothing was transferred or mutated.
    InsufficientTreasury = 7,
    /// A checked arithmetic operation would over- or under-flow, or a
    /// conversion would truncate.
    MathOverflow = 8,
}

/// Storage keys of this module, kept out of the governance `DataKey` union
/// (whose variants share the XDR spec with every other module's keys) and
/// namespaced so they can never collide with `DataKey`, `TimelockKey` or
/// `VeKey` entries.
#[contracttype]
pub enum QfKey {
    /// Persistent: the funding record of bounty `.0`.
    QfBounty(u64),
    /// Persistent: donor `.1`'s cumulative contribution to bounty `.0`.
    QfDonation(u64, Address),
}

/// A bounty's funding state.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BountyFunding {
    /// Address that receives the donations plus the match on completion.
    pub recipient: Address,
    /// Sum of every donor's cumulative contribution, in base units.
    pub total_donated: i128,
    /// Running sum `T = Σ ⌊√(cᵢ · 10¹⁴)⌋`, maintained incrementally at
    /// donation time so completion never iterates over donors.
    pub sqrt_sum_scaled: i128,
    /// Whether the bounty was completed (and paid) — the duplicate-payout
    /// guard, and the donation cutoff.
    pub completed: bool,
    /// The treasury match actually paid at completion; `0` until then.
    pub match_paid: i128,
}

/// Emitted when a bounty is opened.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QfBountyOpened {
    #[topic]
    pub bounty_id: u64,
    pub recipient: Address,
}

/// Emitted on every accepted donation.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QfDonationReceived {
    #[topic]
    pub bounty_id: u64,
    #[topic]
    pub donor: Address,
    /// This call's amount.
    pub amount: i128,
    /// The donor's cumulative contribution after this call.
    pub donor_total: i128,
}

/// Emitted once, when a completed bounty is paid.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QfBountyCompleted {
    #[topic]
    pub bounty_id: u64,
    pub recipient: Address,
    pub total_donated: i128,
    pub match_amount: i128,
    /// `total_donated + match_amount` — what the recipient received.
    pub payout: i128,
}

/// Integer square root of a `u128`, floored.
///
/// Delegates to the shared `crate::math::isqrt` for inputs that fit `u64`
/// (identical floor semantics — one implementation, no drift from the
/// quadratic-*voting* helper) and runs the same Newton–Raphson iteration in
/// `u128` above that range, because the fixed-point scaled inputs below
/// routinely exceed `u64::MAX`.
fn isqrt_u128(n: u128) -> u128 {
    if n == 0 {
        return 0;
    }
    if n <= u64::MAX as u128 {
        return crate::math::isqrt(n as u64) as u128;
    }
    let bit_len = 128 - n.leading_zeros();
    // `2^⌈bit_len/2⌉ ≥ √n`, so the iteration starts above the root and
    // descends monotonically to `⌊√n⌋` (same scheme as `math::isqrt`).
    let mut x: u128 = 1u128 << bit_len.div_ceil(2);
    loop {
        // `x ≥ 1` and, from the initial bound, `n / x ≤ 2^64`, so the sum
        // cannot overflow `u128`.
        let next = (x + n / x) / 2;
        if next >= x {
            break x;
        }
        x = next;
    }
}

/// `⌊√(amount · 10¹⁴)⌋` — `√amount` in fixed point at [`QF_SCALE`].
/// A non-positive amount contributes `0`.
pub(crate) fn scaled_sqrt(amount: i128) -> Result<u128, QfError> {
    if amount <= 0 {
        return Ok(0);
    }
    let amount = u128::try_from(amount).map_err(|_| QfError::MathOverflow)?;
    let scaled = amount
        .checked_mul(QF_SCALE_SQ)
        .ok_or(QfError::MathOverflow)?;
    Ok(isqrt_u128(scaled))
}

/// `⌊T² / 10¹⁴⌋ − total`, floored at every step and **clamped at zero** —
/// the documented rounding rule: the treasury can only ever be under-matched.
pub(crate) fn match_from_scaled(
    sqrt_sum_scaled: u128,
    total_donated: i128,
) -> Result<i128, QfError> {
    let squared = sqrt_sum_scaled
        .checked_mul(sqrt_sum_scaled)
        .ok_or(QfError::MathOverflow)?;
    let scaled_match = squared / QF_SCALE_SQ;
    let total = u128::try_from(total_donated).map_err(|_| QfError::MathOverflow)?;
    let matched = scaled_match.saturating_sub(total);
    i128::try_from(matched).map_err(|_| QfError::MathOverflow)
}

/// Keep a freshly written funding or donation entry out of archival range
/// for as long as the network allows: a bounty's horizon (donations until
/// an unknown completion ledger) is unbounded, so nothing shorter is safe.
fn extend_entry_ttl(env: &Env, key: &QfKey) {
    let ttl = env.storage().max_ttl();
    env.storage().persistent().extend_ttl(key, ttl, ttl);
}

#[contractimpl]
impl Governance {
    /// Open bounty `bounty_id`, payable to `recipient` on completion.
    ///
    /// Gated by `current_contract_address().require_auth()`, like
    /// `set_treasury_token` and `enable_timelock`: the host satisfies that
    /// only when this contract is the direct caller — i.e. the call arrives
    /// through an approved proposal executed against this contract. A
    /// direct external call fails authorization, so bounties can only ever
    /// be created through governance approval.
    ///
    /// Errors: [`QfError::BountyAlreadyExists`] for a duplicate id.
    pub fn qf_open_bounty(env: Env, bounty_id: u64, recipient: Address) -> Result<(), QfError> {
        env.current_contract_address().require_auth();
        let key = QfKey::QfBounty(bounty_id);
        if env.storage().persistent().has(&key) {
            return Err(QfError::BountyAlreadyExists);
        }
        env.storage().persistent().set(
            &key,
            &BountyFunding {
                recipient: recipient.clone(),
                total_donated: 0,
                sqrt_sum_scaled: 0,
                completed: false,
                match_paid: 0,
            },
        );
        extend_entry_ttl(&env, &key);
        QfBountyOpened {
            bounty_id,
            recipient,
        }
        .publish(&env);
        Ok(())
    }

    /// Donate `amount` of the configured `TreasuryToken` to bounty
    /// `bounty_id`, taking custody on this contract.
    ///
    /// Repeat donations from the same `donor` aggregate into one cumulative
    /// contribution per bounty; different bounties are fully independent.
    /// All arithmetic is checked and every check precedes every mutation,
    /// so a rejected donation leaves all state untouched; the token
    /// transfer runs last (effects before interactions), and Soroban's
    /// invocation atomicity rolls everything back if it fails.
    ///
    /// Errors: [`QfError::InvalidAmount`] for a non-positive amount,
    /// [`QfError::BountyNotFound`] for an unknown bounty,
    /// [`QfError::BountyNotOpen`] once the bounty is completed (donation
    /// cutoff), [`QfError::TreasuryNotConfigured`] without a configured
    /// token, [`QfError::MathOverflow`] when the amount or the donor's
    /// cumulative total leaves the fixed-point arithmetic's range.
    pub fn qf_donate(
        env: Env,
        donor: Address,
        bounty_id: u64,
        amount: i128,
    ) -> Result<(), QfError> {
        donor.require_auth();
        if amount <= 0 {
            return Err(QfError::InvalidAmount);
        }
        let token: Address = env
            .storage()
            .instance()
            .get(&DataKey::TreasuryToken)
            .ok_or(QfError::TreasuryNotConfigured)?;

        let bounty_key = QfKey::QfBounty(bounty_id);
        let mut bounty: BountyFunding = env
            .storage()
            .persistent()
            .get(&bounty_key)
            .ok_or(QfError::BountyNotFound)?;
        if bounty.completed {
            return Err(QfError::BountyNotOpen);
        }

        // ── Checks and all checked arithmetic, before any mutation ─────
        let donation_key = QfKey::QfDonation(bounty_id, donor.clone());
        let previous: i128 = env.storage().persistent().get(&donation_key).unwrap_or(0);
        let cumulative = previous.checked_add(amount).ok_or(QfError::MathOverflow)?;
        let t_new = scaled_sqrt(cumulative)?;
        let t_old = scaled_sqrt(previous)?;
        // `isqrt` is monotone, so `t_new ≥ t_old`; the arm is a defensive
        // stated precondition, not a reachable state.
        let sqrt_sum = u128::try_from(bounty.sqrt_sum_scaled)
            .map_err(|_| QfError::MathOverflow)?
            .checked_add(t_new.checked_sub(t_old).ok_or(QfError::MathOverflow)?)
            .ok_or(QfError::MathOverflow)?;
        let total_donated = bounty
            .total_donated
            .checked_add(amount)
            .ok_or(QfError::MathOverflow)?;
        let sqrt_sum_scaled = i128::try_from(sqrt_sum).map_err(|_| QfError::MathOverflow)?;

        // ── Effects ────────────────────────────────────────────────────
        bounty.total_donated = total_donated;
        bounty.sqrt_sum_scaled = sqrt_sum_scaled;
        env.storage().persistent().set(&donation_key, &cumulative);
        env.storage().persistent().set(&bounty_key, &bounty);
        extend_entry_ttl(&env, &donation_key);
        extend_entry_ttl(&env, &bounty_key);

        // ── Interaction ────────────────────────────────────────────────
        token::Client::new(&env, &token).transfer(&donor, env.current_contract_address(), &amount);

        QfDonationReceived {
            bounty_id,
            donor,
            amount,
            donor_total: cumulative,
        }
        .publish(&env);
        Ok(())
    }

    /// Complete bounty `bounty_id`: pay its recipient `total_donated +
    /// match` once, and close the bounty. Returns the treasury match paid.
    ///
    /// Gated by `current_contract_address().require_auth()`, like
    /// `set_treasury_token`: completion — and with it the treasury payout —
    /// is reachable only through an approved proposal, so this module
    /// invents no new completion authority and bypasses no existing one.
    /// The `completed` flag is written **before** the transfer, so not even
    /// a reentrant call can pay the same match twice; any failure after the
    /// flag is written aborts the whole invocation and restores it
    /// (Soroban invocation atomicity), so a failed payout preserves state.
    ///
    /// Errors: [`QfError::BountyNotFound`], [`QfError::BountyAlreadyCompleted`],
    /// [`QfError::TreasuryNotConfigured`], [`QfError::InsufficientTreasury`]
    /// (nothing transferred, nothing mutated), [`QfError::MathOverflow`].
    pub fn qf_complete_bounty(env: Env, bounty_id: u64) -> Result<i128, QfError> {
        env.current_contract_address().require_auth();

        let bounty_key = QfKey::QfBounty(bounty_id);
        let mut bounty: BountyFunding = env
            .storage()
            .persistent()
            .get(&bounty_key)
            .ok_or(QfError::BountyNotFound)?;
        if bounty.completed {
            return Err(QfError::BountyAlreadyCompleted);
        }
        let token: Address = env
            .storage()
            .instance()
            .get(&DataKey::TreasuryToken)
            .ok_or(QfError::TreasuryNotConfigured)?;

        let sqrt_sum_scaled =
            u128::try_from(bounty.sqrt_sum_scaled).map_err(|_| QfError::MathOverflow)?;
        let match_amount = match_from_scaled(sqrt_sum_scaled, bounty.total_donated)?;
        let match_u128 = u128::try_from(match_amount).map_err(|_| QfError::MathOverflow)?;

        // The treasury proper is the contract's balance net of *all*
        // donations it holds for open bounties; that net must cover this
        // match, and the balance must at least cover the principal it is
        // about to pass through.
        let balance = token::Client::new(&env, &token).balance(&env.current_contract_address());
        let balance = u128::try_from(balance).map_err(|_| QfError::MathOverflow)?;
        let donated = u128::try_from(bounty.total_donated).map_err(|_| QfError::MathOverflow)?;
        if balance < donated || match_u128 > balance - donated {
            return Err(QfError::InsufficientTreasury);
        }
        let payout = bounty
            .total_donated
            .checked_add(match_amount)
            .ok_or(QfError::MathOverflow)?;

        // ── Effects (before the external transfer) ─────────────────────
        bounty.completed = true;
        bounty.match_paid = match_amount;
        env.storage().persistent().set(&bounty_key, &bounty);
        extend_entry_ttl(&env, &bounty_key);

        // ── Interaction ────────────────────────────────────────────────
        if payout > 0 {
            token::Client::new(&env, &token).transfer(
                &env.current_contract_address(),
                bounty.recipient.clone(),
                &payout,
            );
        }

        QfBountyCompleted {
            bounty_id,
            recipient: bounty.recipient.clone(),
            total_donated: bounty.total_donated,
            match_amount,
            payout,
        }
        .publish(&env);
        Ok(match_amount)
    }

    /// Read-only: the match `bounty_id` would earn if completed right now.
    /// O(1) — the per-bounty scaled square-root sum is maintained at
    /// donation time. Also the value `qf_complete_bounty` pays (before the
    /// treasury-sufficiency gate).
    pub fn qf_preview_match(env: Env, bounty_id: u64) -> Result<i128, QfError> {
        let bounty: BountyFunding = env
            .storage()
            .persistent()
            .get(&QfKey::QfBounty(bounty_id))
            .ok_or(QfError::BountyNotFound)?;
        let sqrt_sum_scaled =
            u128::try_from(bounty.sqrt_sum_scaled).map_err(|_| QfError::MathOverflow)?;
        match_from_scaled(sqrt_sum_scaled, bounty.total_donated)
    }

    /// Read-only: a bounty's funding record, if it exists.
    pub fn qf_get_bounty(env: Env, bounty_id: u64) -> Option<BountyFunding> {
        env.storage().persistent().get(&QfKey::QfBounty(bounty_id))
    }

    /// Read-only: `donor`'s cumulative contribution to `bounty_id`, or `0`.
    pub fn qf_get_donation(env: Env, bounty_id: u64, donor: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&QfKey::QfDonation(bounty_id, donor))
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use soroban_sdk::{
        testutils::{Address as _, EnvTestConfig, Events as _},
        token::{StellarAssetClient, TokenClient},
        Address, Env, InvokeError, Vec,
    };

    /// 0.1 token in 7-decimal base units. Amounts of the shape `j² · 10⁶`
    /// have exact fixed-point square roots (`√(c · 10¹⁴) = j · 10¹⁰`), so
    /// those tests can pin hand-computed theoretical values.
    const ONE_TENTH: i128 = 1_000_000;
    const FOUR_TENTHS: i128 = 4_000_000;

    struct Harness {
        env: Env,
        gov: GovernanceClient<'static>,
        gov_id: Address,
        token: Address,
        d1: Address,
        d2: Address,
        d3: Address,
    }

    /// Snapshots off: this suite pins observable behaviour with explicit
    /// assertions rather than golden JSON files.
    fn new_env() -> Env {
        Env::new_with_config(EnvTestConfig {
            capture_snapshot_at_drop: false,
        })
    }

    /// One member, a configured treasury token, and three funded donors.
    fn setup() -> Harness {
        let env = new_env();
        env.mock_all_auths();

        let member = Address::generate(&env);
        let members = Vec::from_array(&env, [member]);
        let deposits = Vec::from_array(&env, [1u64]);
        let gov_id = env.register(Governance, (members, deposits, 10_000u32, 100u32));
        let gov = GovernanceClient::new(&env, &gov_id);

        let sac = env.register_stellar_asset_contract_v2(Address::generate(&env));
        let token = sac.address();
        gov.set_treasury_token(&token);

        let d1 = Address::generate(&env);
        let d2 = Address::generate(&env);
        let d3 = Address::generate(&env);
        let sac_admin = StellarAssetClient::new(&env, &token);
        for donor in [&d1, &d2, &d3] {
            sac_admin.mint(donor, &(20 * ONE_TENTH));
        }

        Harness {
            env,
            gov,
            gov_id,
            token,
            d1,
            d2,
            d3,
        }
    }

    /// A governance instance with **no** configured treasury token.
    fn setup_without_treasury() -> (Env, GovernanceClient<'static>, Address) {
        let env = new_env();
        env.mock_all_auths();
        let member = Address::generate(&env);
        let members = Vec::from_array(&env, [member]);
        let deposits = Vec::from_array(&env, [1u64]);
        let gov_id = env.register(Governance, (members, deposits, 10_000u32, 100u32));
        (env.clone(), GovernanceClient::new(&env, &gov_id), gov_id)
    }

    fn token_of(h: &Harness) -> TokenClient<'_> {
        TokenClient::new(&h.env, &h.token)
    }

    // ── A. Donation accounting ─────────────────────────────────────────

    #[test]
    fn open_bounty_records_the_recipient_and_starts_empty() {
        let h = setup();
        h.gov.qf_open_bounty(&7, &h.d1);
        let record = h.gov.qf_get_bounty(&7).unwrap();
        assert_eq!(record.recipient, h.d1);
        assert_eq!(record.total_donated, 0);
        assert_eq!(record.sqrt_sum_scaled, 0);
        assert!(!record.completed);
        assert_eq!(record.match_paid, 0);
        assert_eq!(h.gov.qf_preview_match(&7), 0);
        assert_eq!(h.gov.qf_get_donation(&7, &h.d1), 0);
    }

    #[test]
    fn open_bounty_rejects_duplicate_id() {
        let h = setup();
        h.gov.qf_open_bounty(&7, &h.d1);
        assert_eq!(
            h.gov.try_qf_open_bounty(&7, &h.d2),
            Err(Ok(QfError::BountyAlreadyExists))
        );
        // The first record is untouched by the refused open.
        assert_eq!(h.gov.qf_get_bounty(&7).unwrap().recipient, h.d1);
    }

    #[test]
    fn first_donation_moves_tokens_and_records_totals() {
        let h = setup();
        h.gov.qf_open_bounty(&1, &h.d3);
        let balance_before = token_of(&h).balance(&h.gov_id);

        h.gov.qf_donate(&h.d1, &1, &ONE_TENTH);

        assert_eq!(token_of(&h).balance(&h.d1), 19 * ONE_TENTH);
        assert_eq!(token_of(&h).balance(&h.gov_id), balance_before + ONE_TENTH);
        assert_eq!(h.gov.qf_get_donation(&1, &h.d1), ONE_TENTH);
        let record = h.gov.qf_get_bounty(&1).unwrap();
        assert_eq!(record.total_donated, ONE_TENTH);
        // ⌊√(10⁶ · 10¹⁴)⌋ = 10¹⁰ exactly.
        assert_eq!(record.sqrt_sum_scaled, 10_000_000_000);
    }

    #[test]
    fn repeat_donations_from_one_donor_aggregate() {
        let h = setup();
        h.gov.qf_open_bounty(&1, &h.d3);

        h.gov.qf_donate(&h.d1, &1, &ONE_TENTH);
        h.gov.qf_donate(&h.d1, &1, &(3 * ONE_TENTH));

        assert_eq!(h.gov.qf_get_donation(&1, &h.d1), FOUR_TENTHS);
        let record = h.gov.qf_get_bounty(&1).unwrap();
        assert_eq!(record.total_donated, FOUR_TENTHS);
        // Incremental: t(10⁶) = 10¹⁰, then t(4·10⁶) − t(10⁶) = 10¹⁰, so
        // the running sum is exactly t(4·10⁶) = 2·10¹⁰.
        assert_eq!(record.sqrt_sum_scaled, 20_000_000_000);
        // The custodied balance equals the one donor's cumulative total.
        assert_eq!(token_of(&h).balance(&h.gov_id), FOUR_TENTHS);
    }

    #[test]
    fn donations_from_different_donors_are_tracked_independently() {
        let h = setup();
        h.gov.qf_open_bounty(&1, &h.d3);

        h.gov.qf_donate(&h.d1, &1, &ONE_TENTH);
        h.gov.qf_donate(&h.d2, &1, &FOUR_TENTHS);

        assert_eq!(h.gov.qf_get_donation(&1, &h.d1), ONE_TENTH);
        assert_eq!(h.gov.qf_get_donation(&1, &h.d2), FOUR_TENTHS);
        assert_eq!(h.gov.qf_get_donation(&1, &h.d3), 0);
        let record = h.gov.qf_get_bounty(&1).unwrap();
        assert_eq!(record.total_donated, 5 * ONE_TENTH);
        assert_eq!(record.sqrt_sum_scaled, 30_000_000_000);
    }

    #[test]
    fn donations_to_different_bounties_stay_isolated() {
        let h = setup();
        h.gov.qf_open_bounty(&1, &h.d3);
        h.gov.qf_open_bounty(&2, &h.d3);

        h.gov.qf_donate(&h.d1, &1, &ONE_TENTH);
        h.gov.qf_donate(&h.d2, &2, &FOUR_TENTHS);
        h.gov.qf_donate(&h.d1, &2, &ONE_TENTH);

        assert_eq!(h.gov.qf_get_donation(&1, &h.d1), ONE_TENTH);
        assert_eq!(h.gov.qf_get_donation(&1, &h.d2), 0);
        assert_eq!(h.gov.qf_get_donation(&2, &h.d1), ONE_TENTH);
        assert_eq!(h.gov.qf_get_donation(&2, &h.d2), FOUR_TENTHS);
        assert_eq!(h.gov.qf_get_bounty(&1).unwrap().total_donated, ONE_TENTH);
        assert_eq!(
            h.gov.qf_get_bounty(&2).unwrap().total_donated,
            5 * ONE_TENTH
        );
        // Two donors on bounty 2 ⇒ a real match; one donor on bounty 1 ⇒ none.
        assert_eq!(h.gov.qf_preview_match(&2), 4 * ONE_TENTH);
        assert_eq!(h.gov.qf_preview_match(&1), 0);
    }

    #[test]
    fn invalid_donation_amounts_are_rejected() {
        let h = setup();
        h.gov.qf_open_bounty(&1, &h.d3);
        assert_eq!(
            h.gov.try_qf_donate(&h.d1, &1, &0),
            Err(Ok(QfError::InvalidAmount))
        );
        assert_eq!(
            h.gov.try_qf_donate(&h.d1, &1, &-5),
            Err(Ok(QfError::InvalidAmount))
        );
        assert_eq!(h.gov.qf_get_bounty(&1).unwrap().total_donated, 0);
        assert_eq!(token_of(&h).balance(&h.d1), 20 * ONE_TENTH);
    }

    #[test]
    fn donation_to_unknown_bounty_is_rejected() {
        let h = setup();
        assert_eq!(
            h.gov.try_qf_donate(&h.d1, &42, &ONE_TENTH),
            Err(Ok(QfError::BountyNotFound))
        );
        assert_eq!(
            h.gov.try_qf_preview_match(&42),
            Err(Ok(QfError::BountyNotFound))
        );
        assert!(h.gov.qf_get_bounty(&42).is_none());
    }

    // ── B. Matching calculations ───────────────────────────────────────

    #[test]
    fn no_donations_match_zero() {
        let h = setup();
        let recipient = Address::generate(&h.env);
        h.gov.qf_open_bounty(&1, &recipient);
        assert_eq!(h.gov.qf_preview_match(&1), 0);
        // Completing an unfunded bounty pays nothing and still closes it.
        assert_eq!(h.gov.qf_complete_bounty(&1), 0);
        let record = h.gov.qf_get_bounty(&1).unwrap();
        assert!(record.completed);
        assert_eq!(record.match_paid, 0);
        assert_eq!(token_of(&h).balance(&recipient), 0);
    }

    #[test]
    fn single_donor_always_matches_zero() {
        let h = setup();
        // The sweep below donates ~10¹⁴ stroops from d1 in total; top the
        // donor up well past that sum first.
        StellarAssetClient::new(&h.env, &h.token).mint(&h.d1, &200_000_000_000_000);
        h.gov.qf_open_bounty(&1, &h.d3);
        // A spread of amounts, including non-squares and tiny values:
        // (⌊√c⌋)² ≤ c always, so the clamped match is exactly 0.
        for (i, amount) in [
            1i128,
            2,
            3,
            999_999,
            ONE_TENTH,
            ONE_TENTH + 1,
            FOUR_TENTHS,
            10 * ONE_TENTH,
            1_000_000_000_000,
            100_000_000_000_000,
        ]
        .iter()
        .enumerate()
        {
            let bounty = (i as u64) + 10;
            h.gov.qf_open_bounty(&bounty, &h.d3);
            h.gov.qf_donate(&h.d1, &bounty, amount);
            assert_eq!(h.gov.qf_preview_match(&bounty), 0, "amount {amount}");
        }
    }

    #[test]
    fn equal_contributions_match_the_theoretical_value() {
        let h = setup();
        h.gov.qf_open_bounty(&1, &h.d3);
        h.gov.qf_open_bounty(&2, &h.d3);

        // Two donors of 10⁶: (2·10⁷)²/10¹⁴ − 2·10⁶ = 4·10⁶ − 2·10⁶.
        h.gov.qf_donate(&h.d1, &1, &ONE_TENTH);
        h.gov.qf_donate(&h.d2, &1, &ONE_TENTH);
        assert_eq!(h.gov.qf_preview_match(&1), 2 * ONE_TENTH);

        // Four donors of 10⁶: (4·10⁷)²/10¹⁴ − 4·10⁶ = 16·10⁶ − 4·10⁶.
        h.gov.qf_donate(&h.d1, &2, &ONE_TENTH);
        h.gov.qf_donate(&h.d2, &2, &ONE_TENTH);
        h.gov.qf_donate(&h.d3, &2, &ONE_TENTH);
        let fourth = Address::generate(&h.env);
        StellarAssetClient::new(&h.env, &h.token).mint(&fourth, &ONE_TENTH);
        h.gov.qf_donate(&fourth, &2, &ONE_TENTH);
        assert_eq!(h.gov.qf_preview_match(&2), 12 * ONE_TENTH);
    }

    #[test]
    fn unequal_contributions_match_the_theoretical_value() {
        let h = setup();
        h.gov.qf_open_bounty(&1, &h.d3);

        // Donors of 10⁶ and 4·10⁶: (10⁷ + 2·10⁷)²/10¹⁴ − 5·10⁶
        // = 9·10⁶ − 5·10⁶.
        h.gov.qf_donate(&h.d1, &1, &ONE_TENTH);
        h.gov.qf_donate(&h.d2, &1, &FOUR_TENTHS);
        assert_eq!(h.gov.qf_preview_match(&1), 4 * ONE_TENTH);
    }

    #[test]
    fn same_total_different_distribution_gives_different_match() {
        let h = setup();
        h.gov.qf_open_bounty(&1, &h.d3);
        h.gov.qf_open_bounty(&2, &h.d3);
        let fourth = Address::generate(&h.env);
        StellarAssetClient::new(&h.env, &h.token).mint(&fourth, &(5 * ONE_TENTH));

        // Both bounties receive 4·10⁶ in total… (`fourth` needs 5·10⁶:
        // 4·10⁶ on bounty 1 plus 1·10⁶ on bounty 2).
        h.gov.qf_donate(&fourth, &1, &FOUR_TENTHS);
        h.gov.qf_donate(&h.d1, &2, &ONE_TENTH);
        h.gov.qf_donate(&h.d2, &2, &ONE_TENTH);
        h.gov.qf_donate(&h.d3, &2, &ONE_TENTH);
        h.gov.qf_donate(&fourth, &2, &ONE_TENTH);

        assert_eq!(h.gov.qf_get_bounty(&1).unwrap().total_donated, FOUR_TENTHS);
        assert_eq!(h.gov.qf_get_bounty(&2).unwrap().total_donated, FOUR_TENTHS);
        // …but the crowd-sourced distribution matches 12·10⁶ while the
        // single-donor distribution matches nothing. Donor-level, not
        // total-level.
        assert_eq!(h.gov.qf_preview_match(&1), 0);
        assert_eq!(h.gov.qf_preview_match(&2), 12 * ONE_TENTH);
    }

    #[test]
    fn splitting_increases_the_match() {
        let h = setup();
        // 4·10⁶ as 1 donor (bounty 1), 2+2 (bounty 2), 1+1+1+1 (bounty 3).
        let extra: std::vec::Vec<Address> = std::vec![
            Address::generate(&h.env),
            Address::generate(&h.env),
            Address::generate(&h.env)
        ];
        let sac = StellarAssetClient::new(&h.env, &h.token);
        for a in &extra {
            sac.mint(a, &(4 * ONE_TENTH));
        }
        for id in 1u64..=3 {
            h.gov.qf_open_bounty(&id, &h.d3);
        }
        h.gov.qf_donate(&h.d1, &1, &FOUR_TENTHS);
        h.gov.qf_donate(&h.d1, &2, &(2 * ONE_TENTH));
        h.gov.qf_donate(&extra[0], &2, &(2 * ONE_TENTH));
        h.gov.qf_donate(&h.d1, &3, &ONE_TENTH);
        h.gov.qf_donate(&h.d2, &3, &ONE_TENTH);
        h.gov.qf_donate(&h.d3, &3, &ONE_TENTH);
        h.gov.qf_donate(&extra[0], &3, &ONE_TENTH);

        let lumped = h.gov.qf_preview_match(&1);
        let halved = h.gov.qf_preview_match(&2);
        let quartered = h.gov.qf_preview_match(&3);
        assert_eq!(lumped, 0);
        assert!(lumped < halved, "{lumped} < {halved}");
        assert!(halved < quartered, "{halved} < {quartered}");
        // Quartered is the exact theoretical value: (4·10⁷)²/10¹⁴ − 4·10⁶.
        assert_eq!(quartered, 12 * ONE_TENTH);
    }

    #[test]
    fn tiny_donations_are_not_materially_lost_to_rounding() {
        let h = setup();
        h.gov.qf_open_bounty(&1, &h.d3);
        h.gov.qf_open_bounty(&2, &h.d3);

        // Two 1-stroop donors: exact theory (2·√1)² − 2 = 2 stroops. The
        // fixed-point scale preserves it (unscaled per-root flooring would
        // too here, but for 10⁶-scale non-squares it would not).
        h.gov.qf_donate(&h.d1, &1, &1);
        h.gov.qf_donate(&h.d2, &1, &1);
        assert_eq!(h.gov.qf_preview_match(&1), 2);

        // Three 1-stroop donors: (3·√1)² − 3 = 6 stroops, exact.
        h.gov.qf_donate(&h.d1, &2, &1);
        h.gov.qf_donate(&h.d2, &2, &1);
        h.gov.qf_donate(&h.d3, &2, &1);
        assert_eq!(h.gov.qf_preview_match(&2), 6);
    }

    #[test]
    fn equivalent_distributions_are_order_independent() {
        let h = setup();
        h.gov.qf_open_bounty(&1, &h.d3);
        h.gov.qf_open_bounty(&2, &h.d3);

        // Same multiset {10⁶, 10⁶, 2·10⁶}, donated in different orders by
        // different donors: identical records and identical match.
        h.gov.qf_donate(&h.d1, &1, &(2 * ONE_TENTH));
        h.gov.qf_donate(&h.d2, &1, &ONE_TENTH);
        h.gov.qf_donate(&h.d3, &1, &ONE_TENTH);

        h.gov.qf_donate(&h.d2, &2, &ONE_TENTH);
        h.gov.qf_donate(&h.d1, &2, &ONE_TENTH);
        h.gov.qf_donate(&h.d3, &2, &(2 * ONE_TENTH));

        let a = h.gov.qf_get_bounty(&1).unwrap();
        let b = h.gov.qf_get_bounty(&2).unwrap();
        assert_eq!(a.total_donated, b.total_donated);
        assert_eq!(a.sqrt_sum_scaled, b.sqrt_sum_scaled);
        assert_eq!(h.gov.qf_preview_match(&1), h.gov.qf_preview_match(&2));
        assert!(h.gov.qf_preview_match(&1) > 0);
    }

    #[test]
    fn repeated_donations_equal_one_lump_donation() {
        let h = setup();
        h.gov.qf_open_bounty(&1, &h.d3);
        h.gov.qf_open_bounty(&2, &h.d3);

        // Trickle vs lump, same final per-donor totals on both bounties.
        h.gov.qf_donate(&h.d1, &1, &ONE_TENTH);
        h.gov.qf_donate(&h.d1, &1, &(3 * ONE_TENTH));
        h.gov.qf_donate(&h.d2, &1, &ONE_TENTH);

        h.gov.qf_donate(&h.d1, &2, &FOUR_TENTHS);
        h.gov.qf_donate(&h.d2, &2, &ONE_TENTH);

        let a = h.gov.qf_get_bounty(&1).unwrap();
        let b = h.gov.qf_get_bounty(&2).unwrap();
        assert_eq!(a.total_donated, b.total_donated);
        assert_eq!(a.sqrt_sum_scaled, b.sqrt_sum_scaled);
        assert_eq!(h.gov.qf_preview_match(&1), h.gov.qf_preview_match(&2));
    }

    // ── C. Mathematical correctness ────────────────────────────────────

    #[test]
    fn match_is_never_negative_across_compositions() {
        // Pure-math sweep: every composition of a range of totals into up
        // to four parts. The match is clamped at zero, never negative, and
        // a single part always yields exactly zero.
        // Helper: one composition of `total` must match ≥ 0, and a lone
        // donor must match exactly 0.
        fn check(total: i128, parts: &[i128]) {
            let sqrt_sum: u128 = parts.iter().map(|p| scaled_sqrt(*p).unwrap()).sum();
            let m = match_from_scaled(sqrt_sum, total).unwrap();
            assert!(m >= 0, "negative match for {parts:?}");
            let donors = parts.iter().filter(|p| **p > 0).count();
            if donors <= 1 {
                assert_eq!(m, 0, "single donor matched for {parts:?}");
            }
        }

        // Exhaustive compositions of small totals into up to three parts.
        for total in [1i128, 2, 3, 4, 7, 10, 99, 100, 1_000] {
            for a in 0..=total {
                for b in 0..=(total - a) {
                    let c = total - a - b;
                    check(total, &[a, b, c]);
                }
            }
        }

        // Large totals: exhaustive enumeration would be O(total²), so sweep
        // boundary and interior split points instead of every composition.
        let percent = |total: i128, p: u32| total * i128::from(p) / 100;
        for total in [
            ONE_TENTH,
            ONE_TENTH + 1,
            12_345_678,
            987_654_321,
            100_000_000_000_000,
        ] {
            for pa in [0u32, 1, 25, 50, 75, 99, 100] {
                let a = percent(total, pa);
                let rest = total - a;
                for pb in [0u32, 1, 50, 99] {
                    let b = percent(rest, pb);
                    check(total, &[a, b, rest - b]);
                }
            }
            check(total, &[total]);
        }
    }

    #[test]
    fn isqrt_u128_matches_the_shared_u64_helper() {
        // The u64-range delegation must agree exactly with `math::isqrt`.
        for n in 0..=10_000u64 {
            assert_eq!(isqrt_u128(n as u128), crate::math::isqrt(n) as u128);
        }
        for n in [
            u64::MAX - 1,
            u64::MAX,
            u32::MAX as u64,
            1_000_000_000_000_000,
        ] {
            assert_eq!(isqrt_u128(n as u128), crate::math::isqrt(n) as u128);
        }
        // Above u64: exact on perfect squares, floored in between. The
        // squares of these `k` all exceed `u64::MAX` (so they exercise the
        // u128 Newton path) while still fitting `u128` — `k * k` for
        // `k = 2⁶⁴` would not.
        for k in [
            1u128 << 32,
            (1u128 << 32) + 1,
            1u128 << 40,
            100_000_000_000_000_000,
            u64::MAX as u128,
        ] {
            assert_eq!(isqrt_u128(k * k), k);
            if k > 1 {
                assert_eq!(isqrt_u128(k * k - 1), k - 1);
            }
        }
        // Just above the u64 delegation boundary: ⌊√(2⁶⁴+2)⌋ = 2³², which
        // differs from ⌊√(u64::MAX)⌋ = 2³²−1 and exercises the u128 path.
        assert_eq!(isqrt_u128(u64::MAX as u128 + 2), 1u128 << 32);
        // A u128-range non-square: ⌊√(2⁸⁰−1)⌋ = 2⁴⁰−1.
        assert_eq!(
            isqrt_u128((1u128 << 40) * (1u128 << 40) - 1),
            (1u128 << 40) - 1
        );
    }

    #[test]
    fn deterministic_rounding_on_non_square_amounts() {
        // c = 2 stroops: √2 · 10⁷ = 14142135.62…, so the scaled term
        // floors to 14142135 and the single-donor match floors to 0.
        let t = scaled_sqrt(2).unwrap();
        assert_eq!(t, 14_142_135);
        assert_eq!(match_from_scaled(t, 2).unwrap(), 0);
        // Two such donors: T = 2·14142135, T² = 799999929432900, so
        // ⌊T²/10¹⁴⌋ = 7 and the match floors to 3 — under the exact 4
        // ((2√2)² − 4) because the roots floored, never over it.
        assert_eq!(match_from_scaled(2 * t, 4).unwrap(), 3);
        // Re-running the same computation yields the same result.
        assert_eq!(match_from_scaled(2 * t, 4).unwrap(), 3);
    }

    #[test]
    fn donation_amount_outside_fixed_point_range_is_typed() {
        let h = setup();
        h.gov.qf_open_bounty(&1, &h.d3);

        // The largest amount whose scaled square root is representable.
        let max_single = (u128::MAX / QF_SCALE_SQ) as i128;
        let sac = StellarAssetClient::new(&h.env, &h.token);
        sac.mint(&h.d1, &max_single);
        assert_eq!(
            h.gov.try_qf_donate(&h.d1, &1, &(max_single + 1)),
            Err(Ok(QfError::MathOverflow))
        );
        h.gov.qf_donate(&h.d1, &1, &max_single);
        assert_eq!(h.gov.qf_get_donation(&1, &h.d1), max_single);
        // A single donor still matches zero at the boundary.
        assert_eq!(h.gov.qf_preview_match(&1), 0);
        // The next stroop would push the cumulative total out of range.
        assert_eq!(
            h.gov.try_qf_donate(&h.d1, &1, &1),
            Err(Ok(QfError::MathOverflow))
        );
        assert_eq!(h.gov.qf_get_donation(&1, &h.d1), max_single);
    }

    #[test]
    fn match_squaring_overflow_is_typed_not_a_trap() {
        let h = setup();
        // Two donors, each at half the per-donation ceiling: their scaled
        // roots sum above √(u128::MAX), so T² is not representable.
        let half = (u128::MAX / QF_SCALE_SQ / 2) as i128;
        for id in 1u64..=2 {
            h.gov.qf_open_bounty(&id, &h.d3);
        }
        let sac = StellarAssetClient::new(&h.env, &h.token);
        sac.mint(&h.d1, &half);
        sac.mint(&h.d2, &half);
        h.gov.qf_donate(&h.d1, &1, &half);
        h.gov.qf_donate(&h.d2, &1, &half);
        assert_eq!(
            h.gov.try_qf_preview_match(&1),
            Err(Ok(QfError::MathOverflow))
        );
        // An untouched bounty next door is unaffected.
        assert_eq!(h.gov.qf_preview_match(&2), 0);
    }

    // ── D. Treasury behaviour ──────────────────────────────────────────

    #[test]
    fn completion_pays_donations_plus_match_exactly_once() {
        let h = setup();
        // A fresh recipient (the harness's donors start pre-funded, so a
        // pre-existing balance would blur the payout assertion).
        let recipient = Address::generate(&h.env);
        h.gov.qf_open_bounty(&1, &recipient);
        let sac = StellarAssetClient::new(&h.env, &h.token);
        sac.mint(&h.gov_id, &(5 * ONE_TENTH));

        h.gov.qf_donate(&h.d1, &1, &ONE_TENTH);
        h.gov.qf_donate(&h.d2, &1, &ONE_TENTH);
        let preview = h.gov.qf_preview_match(&1);
        assert_eq!(preview, 2 * ONE_TENTH);

        // Preview and payout agree.
        assert_eq!(h.gov.qf_complete_bounty(&1), preview);

        // Recipient got principal + match; the treasury paid only the match.
        assert_eq!(token_of(&h).balance(&recipient), 4 * ONE_TENTH);
        assert_eq!(token_of(&h).balance(&h.gov_id), 3 * ONE_TENTH);
        let record = h.gov.qf_get_bounty(&1).unwrap();
        assert!(record.completed);
        assert_eq!(record.match_paid, 2 * ONE_TENTH);
        assert_eq!(record.total_donated, 2 * ONE_TENTH);

        // Duplicate payout is rejected and changes nothing.
        assert_eq!(
            h.gov.try_qf_complete_bounty(&1),
            Err(Ok(QfError::BountyAlreadyCompleted))
        );
        assert_eq!(token_of(&h).balance(&recipient), 4 * ONE_TENTH);
    }

    #[test]
    fn completed_bounty_refuses_further_donations() {
        let h = setup();
        h.gov.qf_open_bounty(&1, &h.d3);
        h.gov.qf_donate(&h.d1, &1, &ONE_TENTH);
        h.gov.qf_complete_bounty(&1);

        assert_eq!(
            h.gov.try_qf_donate(&h.d2, &1, &ONE_TENTH),
            Err(Ok(QfError::BountyNotOpen))
        );
        assert_eq!(h.gov.qf_get_donation(&1, &h.d2), 0);
        assert_eq!(h.gov.qf_get_bounty(&1).unwrap().total_donated, ONE_TENTH);
    }

    #[test]
    fn insufficient_treasury_rejects_atomically_then_succeeds_when_funded() {
        let h = setup();
        let recipient = Address::generate(&h.env);
        h.gov.qf_open_bounty(&1, &recipient);

        h.gov.qf_donate(&h.d1, &1, &ONE_TENTH);
        h.gov.qf_donate(&h.d2, &1, &ONE_TENTH);
        // The match is 2·10⁶; the treasury holds one stroop short of it.
        let sac = StellarAssetClient::new(&h.env, &h.token);
        sac.mint(&h.gov_id, &(2 * ONE_TENTH - 1));

        assert_eq!(
            h.gov.try_qf_complete_bounty(&1),
            Err(Ok(QfError::InsufficientTreasury))
        );
        // Failed payout preserved every piece of state.
        let record = h.gov.qf_get_bounty(&1).unwrap();
        assert!(!record.completed);
        assert_eq!(record.match_paid, 0);
        assert_eq!(token_of(&h).balance(&recipient), 0);
        assert_eq!(h.gov.qf_preview_match(&1), 2 * ONE_TENTH);

        // One stroop more and the same call succeeds.
        sac.mint(&h.gov_id, &1);
        assert_eq!(h.gov.qf_complete_bounty(&1), 2 * ONE_TENTH);
        assert_eq!(token_of(&h).balance(&recipient), 4 * ONE_TENTH);
    }

    #[test]
    fn unauthorized_completion_is_refused_and_state_preserved() {
        let h = setup();
        let recipient = Address::generate(&h.env);
        h.gov.qf_open_bounty(&1, &recipient);
        h.gov.qf_donate(&h.d1, &1, &ONE_TENTH);

        // Drop the auth mock: the contract-address `require_auth()` must
        // fail, exactly as it would for any direct external caller.
        h.env.set_auths(&[]);
        assert_eq!(
            h.gov.try_qf_complete_bounty(&1),
            Err(Err(InvokeError::Abort))
        );

        let record = h.gov.qf_get_bounty(&1).unwrap();
        assert!(!record.completed);
        assert_eq!(record.match_paid, 0);
        assert_eq!(token_of(&h).balance(&recipient), 0);
        assert_eq!(token_of(&h).balance(&h.gov_id), ONE_TENTH);
    }

    #[test]
    fn unauthorized_open_and_donation_gate_are_enforced() {
        let h = setup();
        h.gov.qf_open_bounty(&1, &h.d3);
        h.env.set_auths(&[]);
        // `qf_open_bounty` / `qf_complete_bounty` need the contract's own
        // authorization; only `qf_donate` needs the donor's.
        assert_eq!(
            h.gov.try_qf_open_bounty(&2, &h.d3),
            Err(Err(InvokeError::Abort))
        );
        assert!(h.gov.qf_get_bounty(&2).is_none());
        // And the donor's own authorization cannot be skipped either.
        assert_eq!(
            h.gov.try_qf_donate(&h.d1, &1, &ONE_TENTH),
            Err(Err(InvokeError::Abort))
        );
        assert_eq!(h.gov.qf_get_donation(&1, &h.d1), 0);
    }

    #[test]
    fn unconfigured_treasury_rejects_donation_and_completion() {
        let (env, gov, _gov_id) = setup_without_treasury();
        gov.qf_open_bounty(&1, &Address::generate(&env));
        let donor = Address::generate(&env);
        assert_eq!(
            gov.try_qf_donate(&donor, &1, &ONE_TENTH),
            Err(Ok(QfError::TreasuryNotConfigured))
        );
        assert_eq!(
            gov.try_qf_complete_bounty(&1),
            Err(Ok(QfError::TreasuryNotConfigured))
        );
    }

    #[test]
    fn completing_unknown_bounty_is_rejected() {
        let h = setup();
        assert_eq!(
            h.gov.try_qf_complete_bounty(&99),
            Err(Ok(QfError::BountyNotFound))
        );
    }

    #[test]
    fn each_lifecycle_step_emits_exactly_one_event() {
        let h = setup();
        h.gov.qf_open_bounty(&1, &h.d3);
        assert_eq!(
            h.env
                .events()
                .all()
                .filter_by_contract(&h.gov_id)
                .events()
                .len(),
            1
        );

        h.gov.qf_donate(&h.d1, &1, &ONE_TENTH);
        assert_eq!(
            h.env
                .events()
                .all()
                .filter_by_contract(&h.gov_id)
                .events()
                .len(),
            1
        );

        h.gov.qf_complete_bounty(&1);
        assert_eq!(
            h.env
                .events()
                .all()
                .filter_by_contract(&h.gov_id)
                .events()
                .len(),
            1
        );
    }

    // ── E. Voting is untouched ─────────────────────────────────────────

    #[test]
    fn quadratic_funding_does_not_touch_voting_weights() {
        let h = setup();
        let member = h.gov.qf_get_bounty(&0); // no bounty 0 exists
        assert!(member.is_none());
        // The member's deposit and weight are exactly what the constructor
        // set: 1 deposited → weight ⌊√1⌋ = 1.
        assert_eq!(h.gov.get_total_weight(), 1);

        h.gov.qf_open_bounty(&1, &h.d3);
        h.gov.qf_donate(&h.d1, &1, &(4 * ONE_TENTH));
        h.gov.qf_donate(&h.d2, &1, &ONE_TENTH);

        // Donations moved tokens, not voting power.
        assert_eq!(h.gov.get_total_weight(), 1);
        assert_eq!(h.gov.get_total_deposits(), 1);
        assert_eq!(h.gov.get_member_deposit(&h.d1), 0);
        assert!(!h.gov.is_member(&h.d1));
    }
}
