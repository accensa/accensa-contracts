//! Time-weighted vote escrow (veToken) for governance (issue #438).
//!
//! A member locks governance tokens for a period they choose, from one month
//! up to four years, and the lock adds voting power that **decays linearly**
//! to zero as the unlock ledger approaches:
//!
//! ```text
//! voting_power = locked_amount * remaining_ledgers / MAX_LOCK_LEDGERS
//! ```
//!
//! A lock for the full four years therefore counts for its whole amount, a
//! two-year lock for half of it, and every lock counts for less each ledger.
//! The tokens stay in this contract until the unlock ledger; there is no
//! early withdrawal.
//!
//! # How it reaches a ballot
//!
//! [`voting_power`] feeds `voting::quadratic_weight` as extra *input*: a
//! member's weight is `isqrt(deposit + voting_power)`. The formula is
//! unchanged, and a member without a lock gets exactly the weight they had
//! before this module existed. Only registered members vote, so a lock never
//! makes an outsider a member.
//!
//! # What this does not do
//!
//! `TotalWeight` is fixed when the contract is built and is not recomputed as
//! locks are created, decay or expire, so the quorum denominator does not
//! follow escrowed weight. Escrow also changes a member's weight over time,
//! which `ragequit` and the equal-weight rule of `vote_anonymous` read. Those
//! are called out in the pull request rather than changed here.
//!
//! # Configuration
//!
//! The escrowed token is set once by [`Governance::set_ve_token`], gated like
//! `set_treasury_token` by the contract's own authorization.

use soroban_sdk::{contracterror, contractevent, contractimpl, contracttype, token, Address, Env};

use crate::{Governance, GovernanceArgs, GovernanceClient};

/// Ledgers per day at about five seconds per ledger.
const LEDGERS_PER_DAY: u32 = 17_280;

/// Shortest lock: 30 days (about one month).
pub const MIN_LOCK_LEDGERS: u32 = 30 * LEDGERS_PER_DAY;

/// Longest lock, and the denominator of the power formula: four years.
pub const MAX_LOCK_LEDGERS: u32 = 4 * 365 * LEDGERS_PER_DAY;

/// Ledgers a lock entry stays readable beyond its unlock ledger.
const LOCK_TTL_GRACE: u32 = LEDGERS_PER_DAY;

/// Errors of the escrow functions. Local to this module so that the
/// governance `Error` enum, and with it the spec every deployment carries,
/// does not grow for an optional feature.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum VeError {
    /// No escrow token has been configured.
    TokenNotConfigured = 1,
    /// The escrow token is already configured and cannot change while locks
    /// may exist.
    TokenAlreadySet = 2,
    /// The amount is not strictly positive.
    InvalidAmount = 3,
    /// The lock would be shorter than [`MIN_LOCK_LEDGERS`].
    LockTooShort = 4,
    /// The lock would be longer than [`MAX_LOCK_LEDGERS`].
    LockTooLong = 5,
    /// The account already has a lock; withdraw it first.
    AlreadyLocked = 6,
    /// The account has no lock.
    NoLock = 7,
    /// The unlock ledger has not been reached.
    LockNotExpired = 8,
}

/// Storage keys of this module, kept out of the governance `DataKey` union.
#[contracttype]
pub enum VeKey {
    /// Instance: the escrowed SEP-41 token.
    Token,
    /// Persistent: the lock of an account.
    Lock(Address),
}

/// One account's escrow.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VeLock {
    /// Tokens held by this contract for the lock.
    pub amount: i128,
    /// Ledger at which the lock was created.
    pub start_ledger: u32,
    /// First ledger at which the tokens can be withdrawn.
    pub unlock_ledger: u32,
}

/// Emitted when tokens are locked.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VeLocked {
    #[topic]
    pub user: Address,
    pub amount: i128,
    pub unlock_ledger: u32,
}

/// Emitted when an expired lock is withdrawn.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VeWithdrawn {
    #[topic]
    pub user: Address,
    pub amount: i128,
}

/// `amount * remaining / MAX_LOCK_LEDGERS`, without overflow.
///
/// `remaining` is clamped to [`MAX_LOCK_LEDGERS`]. The product is split so it
/// cannot overflow for any `i128` amount, and the result saturates at
/// `u64::MAX`, the type voting weights use. A non-positive amount has no power.
pub fn power_for(amount: i128, remaining: u32) -> u64 {
    if amount <= 0 || remaining == 0 {
        return 0;
    }
    let remaining = i128::from(remaining.min(MAX_LOCK_LEDGERS));
    let max = i128::from(MAX_LOCK_LEDGERS);
    // amount = q * max + r, so amount * remaining / max = q * remaining + r * remaining / max.
    let power = (amount / max) * remaining + (amount % max) * remaining / max;
    u64::try_from(power).unwrap_or(u64::MAX)
}

/// The current voting power of `lock`: zero once it has expired.
pub fn lock_power(env: &Env, lock: &VeLock) -> u64 {
    let remaining = lock.unlock_ledger.saturating_sub(env.ledger().sequence());
    power_for(lock.amount, remaining)
}

/// Voting power `account` gets from its escrow at this ledger, or `0`.
pub fn voting_power(env: &Env, account: &Address) -> u64 {
    env.storage()
        .persistent()
        .get::<_, VeLock>(&VeKey::Lock(account.clone()))
        .map_or(0, |lock| lock_power(env, &lock))
}

fn escrow_token(env: &Env) -> Result<Address, VeError> {
    env.storage()
        .instance()
        .get(&VeKey::Token)
        .ok_or(VeError::TokenNotConfigured)
}

#[contractimpl]
impl Governance {
    /// Configure the token members escrow. Settable once.
    ///
    /// Gated by `current_contract_address().require_auth()`, like
    /// `set_treasury_token`: it is meant to be reached by an approved
    /// proposal.
    pub fn set_ve_token(env: Env, token: Address) -> Result<(), VeError> {
        env.current_contract_address().require_auth();
        if env.storage().instance().has(&VeKey::Token) {
            return Err(VeError::TokenAlreadySet);
        }
        env.storage().instance().set(&VeKey::Token, &token);
        Ok(())
    }

    /// The escrowed token, if configured.
    pub fn get_ve_token(env: Env) -> Option<Address> {
        env.storage().instance().get(&VeKey::Token)
    }

    /// Lock `amount` tokens until `unlock_ledger`.
    ///
    /// The lock must last at least [`MIN_LOCK_LEDGERS`] and at most
    /// [`MAX_LOCK_LEDGERS`] from the current ledger, and an account holds one
    /// lock at a time. The tokens move from `user` to this contract.
    pub fn create_lock(
        env: Env,
        user: Address,
        amount: i128,
        unlock_ledger: u32,
    ) -> Result<(), VeError> {
        user.require_auth();
        if amount <= 0 {
            return Err(VeError::InvalidAmount);
        }
        let token_address = escrow_token(&env)?;

        let key = VeKey::Lock(user.clone());
        if env.storage().persistent().has(&key) {
            return Err(VeError::AlreadyLocked);
        }

        let now = env.ledger().sequence();
        let duration = unlock_ledger.saturating_sub(now);
        if duration < MIN_LOCK_LEDGERS {
            return Err(VeError::LockTooShort);
        }
        if duration > MAX_LOCK_LEDGERS {
            return Err(VeError::LockTooLong);
        }

        // Effects before the external transfer.
        env.storage().persistent().set(
            &key,
            &VeLock {
                amount,
                start_ledger: now,
                unlock_ledger,
            },
        );
        let ttl = duration
            .saturating_add(LOCK_TTL_GRACE)
            .min(env.storage().max_ttl());
        env.storage().persistent().extend_ttl(&key, ttl, ttl);

        token::Client::new(&env, &token_address).transfer(
            &user,
            env.current_contract_address(),
            &amount,
        );

        VeLocked {
            user,
            amount,
            unlock_ledger,
        }
        .publish(&env);
        Ok(())
    }

    /// Take the tokens of an expired lock back. There is no early exit.
    pub fn withdraw_lock(env: Env, user: Address) -> Result<(), VeError> {
        user.require_auth();
        let key = VeKey::Lock(user.clone());
        let lock: VeLock = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(VeError::NoLock)?;
        if env.ledger().sequence() < lock.unlock_ledger {
            return Err(VeError::LockNotExpired);
        }
        let token_address = escrow_token(&env)?;

        // Effects before the external transfer.
        env.storage().persistent().remove(&key);
        token::Client::new(&env, &token_address).transfer(
            &env.current_contract_address(),
            &user,
            &lock.amount,
        );

        VeWithdrawn {
            user,
            amount: lock.amount,
        }
        .publish(&env);
        Ok(())
    }

    /// The escrow of `user`, if any.
    pub fn get_lock(env: Env, user: Address) -> Option<VeLock> {
        env.storage().persistent().get(&VeKey::Lock(user))
    }

    /// `user`'s voting power from escrow at this ledger. Zero without a lock
    /// and once a lock has expired.
    pub fn get_voting_power(env: Env, user: Address) -> u64 {
        voting_power(&env, &user)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::Error;
    use soroban_sdk::{
        contract, contractimpl,
        testutils::{Address as _, Ledger},
        token::{StellarAssetClient, TokenClient},
        IntoVal, Symbol, Val, Vec,
    };

    const START: u32 = 1_000;
    const YEAR: u32 = 365 * LEDGERS_PER_DAY;

    #[contract]
    struct Target;

    #[contractimpl]
    impl Target {
        pub fn noop(_env: Env, _value: u32) {}
    }

    struct Harness {
        env: Env,
        gov: GovernanceClient<'static>,
        token: Address,
        target: Address,
        member: Address,
        other: Address,
    }

    /// Two members: `member` (deposit 4, weight 2) and `other` (deposit 1,
    /// weight 1). The escrow token is configured and `member` holds tokens.
    fn setup() -> Harness {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| {
            l.sequence_number = START;
            l.min_persistent_entry_ttl = 100;
            l.max_entry_ttl = 6_000_000;
        });

        let member = Address::generate(&env);
        let other = Address::generate(&env);
        let members = Vec::from_array(&env, [member.clone(), other.clone()]);
        let deposits = Vec::from_array(&env, [4u64, 1u64]);
        let gov_id = env.register(Governance, (members, deposits, 5000u32, 100u32));
        let gov = GovernanceClient::new(&env, &gov_id);

        let sac = env.register_stellar_asset_contract_v2(Address::generate(&env));
        let token = sac.address();
        StellarAssetClient::new(&env, &token).mint(&member, &1_000_000_000i128);
        gov.set_ve_token(&token);

        let target = env.register(Target, ());
        Harness {
            env,
            gov,
            token,
            target,
            member,
            other,
        }
    }

    fn advance_to(h: &Harness, sequence: u32) {
        h.env.ledger().with_mut(|l| l.sequence_number = sequence);
    }

    fn balance(h: &Harness, who: &Address) -> i128 {
        TokenClient::new(&h.env, &h.token).balance(who)
    }

    // ── the formula ─────────────────────────────────────────────────────────

    #[test]
    fn power_is_amount_times_remaining_over_max() {
        let amount = 1_000_000_i128;
        assert_eq!(power_for(amount, MAX_LOCK_LEDGERS), 1_000_000);
        assert_eq!(power_for(amount, MAX_LOCK_LEDGERS / 2), 500_000);
        assert_eq!(power_for(amount, MAX_LOCK_LEDGERS / 4), 250_000);
        assert_eq!(power_for(amount, 0), 0);
        assert_eq!(
            power_for(amount, MIN_LOCK_LEDGERS),
            1_000_000 * u64::from(MIN_LOCK_LEDGERS) / u64::from(MAX_LOCK_LEDGERS)
        );
    }

    #[test]
    fn power_never_exceeds_the_amount_and_never_overflows() {
        // More remaining than the maximum is clamped to the maximum.
        assert_eq!(power_for(500, MAX_LOCK_LEDGERS + 1), 500);
        assert_eq!(power_for(500, u32::MAX), 500);
        // The largest amount saturates at the weight type instead of wrapping.
        assert_eq!(power_for(i128::MAX, MAX_LOCK_LEDGERS), u64::MAX);
        // Even one remaining ledger of the largest amount exceeds the weight type.
        assert_eq!(power_for(i128::MAX, 1), u64::MAX);
        // No power without a positive amount.
        assert_eq!(power_for(0, MAX_LOCK_LEDGERS), 0);
        assert_eq!(power_for(-5, MAX_LOCK_LEDGERS), 0);
    }

    #[test]
    fn power_is_monotonic_in_the_remaining_time() {
        let mut last = 0;
        for step in 0..=20u32 {
            let power = power_for(7_777_777, step * (MAX_LOCK_LEDGERS / 20));
            assert!(
                power >= last,
                "power must not shrink as remaining time grows"
            );
            last = power;
        }
    }

    // ── locking ─────────────────────────────────────────────────────────────

    #[test]
    fn locking_moves_the_tokens_into_custody_and_records_the_lock() {
        let h = setup();
        let unlock = START + YEAR;
        h.gov.create_lock(&h.member, &600i128, &unlock);

        assert_eq!(balance(&h, &h.member), 1_000_000_000 - 600);
        assert_eq!(balance(&h, &h.gov.address), 600);
        let lock = h.gov.get_lock(&h.member).unwrap();
        assert_eq!(
            lock,
            VeLock {
                amount: 600,
                start_ledger: START,
                unlock_ledger: unlock
            }
        );
    }

    #[test]
    fn the_duration_bounds_are_exact() {
        let h = setup();
        let account = |h: &Harness| {
            let a = Address::generate(&h.env);
            StellarAssetClient::new(&h.env, &h.token).mint(&a, &1_000i128);
            a
        };
        let (a, b, c, d) = (account(&h), account(&h), account(&h), account(&h));

        assert_eq!(
            h.gov
                .try_create_lock(&a, &100i128, &(START + MIN_LOCK_LEDGERS - 1)),
            Err(Ok(VeError::LockTooShort))
        );
        assert!(h
            .gov
            .try_create_lock(&b, &100i128, &(START + MIN_LOCK_LEDGERS))
            .is_ok());
        assert!(h
            .gov
            .try_create_lock(&c, &100i128, &(START + MAX_LOCK_LEDGERS))
            .is_ok());
        assert_eq!(
            h.gov
                .try_create_lock(&d, &100i128, &(START + MAX_LOCK_LEDGERS + 1)),
            Err(Ok(VeError::LockTooLong))
        );
        // An unlock ledger in the past is a lock that is too short.
        assert_eq!(
            h.gov.try_create_lock(&d, &100i128, &(START - 1)),
            Err(Ok(VeError::LockTooShort))
        );
    }

    #[test]
    fn invalid_requests_are_refused_without_moving_tokens() {
        let h = setup();
        let unlock = START + YEAR;
        for amount in [0i128, -1] {
            assert_eq!(
                h.gov.try_create_lock(&h.member, &amount, &unlock),
                Err(Ok(VeError::InvalidAmount))
            );
        }
        assert_eq!(balance(&h, &h.member), 1_000_000_000);

        h.gov.create_lock(&h.member, &10i128, &unlock);
        assert_eq!(
            h.gov.try_create_lock(&h.member, &10i128, &unlock),
            Err(Ok(VeError::AlreadyLocked))
        );
        assert_eq!(balance(&h, &h.member), 1_000_000_000 - 10);
    }

    #[test]
    fn the_token_is_configured_once_and_required_for_locking() {
        let h = setup();
        assert_eq!(h.gov.get_ve_token(), Some(h.token.clone()));
        assert_eq!(
            h.gov.try_set_ve_token(&Address::generate(&h.env)),
            Err(Ok(VeError::TokenAlreadySet))
        );

        // A governance contract without a token refuses locks.
        let bare_id = h.env.register(
            Governance,
            (
                Vec::from_array(&h.env, [h.member.clone()]),
                Vec::from_array(&h.env, [4u64]),
                5000u32,
                100u32,
            ),
        );
        let bare = GovernanceClient::new(&h.env, &bare_id);
        assert_eq!(
            bare.try_create_lock(&h.member, &10i128, &(START + YEAR)),
            Err(Ok(VeError::TokenNotConfigured))
        );
    }

    // ── decay and withdrawal ────────────────────────────────────────────────

    #[test]
    fn power_decays_linearly_to_zero_at_the_unlock_ledger() {
        let h = setup();
        let amount = 4 * i128::from(MAX_LOCK_LEDGERS);
        StellarAssetClient::new(&h.env, &h.token).mint(&h.member, &amount);
        let unlock = START + MAX_LOCK_LEDGERS;
        h.gov.create_lock(&h.member, &amount, &unlock);

        // Full lock: full power. After a quarter of the time, three quarters.
        assert_eq!(h.gov.get_voting_power(&h.member), amount as u64);
        advance_to(&h, START + MAX_LOCK_LEDGERS / 4);
        assert_eq!(h.gov.get_voting_power(&h.member), (amount as u64) / 4 * 3);
        advance_to(&h, START + MAX_LOCK_LEDGERS / 2);
        assert_eq!(h.gov.get_voting_power(&h.member), (amount as u64) / 2);
        advance_to(&h, unlock - 1);
        assert_eq!(h.gov.get_voting_power(&h.member), 4);
        advance_to(&h, unlock);
        assert_eq!(h.gov.get_voting_power(&h.member), 0);
        advance_to(&h, unlock + 1_000);
        assert_eq!(h.gov.get_voting_power(&h.member), 0);
    }

    #[test]
    fn early_withdrawal_is_refused_and_the_exact_unlock_ledger_pays_out() {
        let h = setup();
        let unlock = START + YEAR;
        h.gov.create_lock(&h.member, &900i128, &unlock);

        for ledger in [START, START + YEAR / 2, unlock - 1] {
            advance_to(&h, ledger);
            assert_eq!(
                h.gov.try_withdraw_lock(&h.member),
                Err(Ok(VeError::LockNotExpired))
            );
        }
        assert_eq!(balance(&h, &h.gov.address), 900);
        assert!(h.gov.get_lock(&h.member).is_some());

        advance_to(&h, unlock);
        h.gov.withdraw_lock(&h.member);
        assert_eq!(balance(&h, &h.member), 1_000_000_000);
        assert_eq!(balance(&h, &h.gov.address), 0);
        assert!(h.gov.get_lock(&h.member).is_none());
        assert_eq!(h.gov.get_voting_power(&h.member), 0);

        assert_eq!(h.gov.try_withdraw_lock(&h.member), Err(Ok(VeError::NoLock)));
        // After withdrawing, the account can lock again.
        h.gov.create_lock(&h.member, &1i128, &(unlock + YEAR));
    }

    #[test]
    fn locks_of_different_accounts_do_not_interfere() {
        let h = setup();
        StellarAssetClient::new(&h.env, &h.token).mint(&h.other, &500i128);
        h.gov.create_lock(&h.member, &300i128, &(START + YEAR));
        h.gov.create_lock(&h.other, &200i128, &(START + 2 * YEAR));
        assert_eq!(h.gov.get_lock(&h.member).unwrap().amount, 300);
        assert_eq!(h.gov.get_lock(&h.other).unwrap().amount, 200);
        assert_eq!(balance(&h, &h.gov.address), 500);

        advance_to(&h, START + YEAR);
        h.gov.withdraw_lock(&h.member);
        assert_eq!(balance(&h, &h.gov.address), 200);
        assert!(h.gov.get_lock(&h.other).is_some());
    }

    // ── ballots ─────────────────────────────────────────────────────────────

    fn propose(h: &Harness, proposer: &Address) -> u64 {
        let args: Vec<Val> = Vec::from_array(&h.env, [1u32.into_val(&h.env)]);
        h.gov
            .propose(proposer, &h.target, &Symbol::new(&h.env, "noop"), &args)
    }

    #[test]
    fn without_a_lock_the_weight_is_the_plain_quadratic_weight() {
        let h = setup();
        assert_eq!(h.gov.get_member_weight(&h.member), 2); // isqrt(4)
        assert_eq!(h.gov.get_member_weight(&h.other), 1);
        let id = propose(&h, &h.member);
        h.gov.vote(&h.member, &id, &true);
        assert_eq!(h.gov.get_proposal(&id).yes_weight, 2);
    }

    #[test]
    fn escrowed_power_raises_the_ballot_weight_and_decays_with_it() {
        let h = setup();
        // Power equals the amount while the lock is at its full length.
        let amount = 12i128;
        h.gov
            .create_lock(&h.member, &amount, &(START + MAX_LOCK_LEDGERS));

        // deposit 4 + power 12 = 16, so the weight is isqrt(16) = 4.
        assert_eq!(h.gov.get_member_weight(&h.member), 4);
        let id = propose(&h, &h.member);
        h.gov.vote(&h.member, &id, &true);
        assert_eq!(h.gov.get_proposal(&id).yes_weight, 4);

        // Half the lock later the power is 6, so the weight is isqrt(10) = 3.
        advance_to(&h, START + MAX_LOCK_LEDGERS / 2);
        assert_eq!(h.gov.get_voting_power(&h.member), 6);
        assert_eq!(h.gov.get_member_weight(&h.member), 3);
        let later = propose(&h, &h.member);
        h.gov.vote(&h.member, &later, &true);
        assert_eq!(h.gov.get_proposal(&later).yes_weight, 3);

        // Once expired the weight is back to the deposit alone.
        advance_to(&h, START + MAX_LOCK_LEDGERS);
        assert_eq!(h.gov.get_member_weight(&h.member), 2);
    }

    #[test]
    fn a_lock_never_makes_an_outsider_a_member() {
        let h = setup();
        let outsider = Address::generate(&h.env);
        StellarAssetClient::new(&h.env, &h.token).mint(&outsider, &1_000i128);
        h.gov
            .create_lock(&outsider, &1_000i128, &(START + MAX_LOCK_LEDGERS));

        assert!(!h.gov.is_member(&outsider));
        assert_eq!(h.gov.get_member_weight(&outsider), 0);
        let id = propose(&h, &h.member);
        assert_eq!(
            h.gov.try_vote(&outsider, &id, &true),
            Err(Ok(Error::NotAMember))
        );
    }

    #[test]
    fn locking_and_withdrawing_each_emit_one_governance_event() {
        use soroban_sdk::testutils::Events as _;
        let h = setup();
        h.gov
            .create_lock(&h.member, &50i128, &(START + MIN_LOCK_LEDGERS));
        let locked = h.env.events().all().filter_by_contract(&h.gov.address);
        assert_eq!(locked.events().len(), 1);

        advance_to(&h, START + MIN_LOCK_LEDGERS);
        h.gov.withdraw_lock(&h.member);
        let withdrawn = h.env.events().all().filter_by_contract(&h.gov.address);
        assert_eq!(withdrawn.events().len(), 1);
    }
}
