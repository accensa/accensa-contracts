//! Daily cumulative spending limits for sub-threshold signers (issue #413).
//!
//! Governance (the full `threshold`) configures a per-token daily allowance
//! with [`set_daily_limit`]. After that, a call authorized by *fewer* than
//! `threshold` registered signers is still accepted by `__check_auth` when it
//! is a routine token spend: every authorized context must be a
//! `transfer(from = this account, to, amount)` on a token with a configured
//! limit, and each attached signer's spending for the current 24-hour window
//! plus the amount must stay within that limit.
//!
//! Anything else — a non-transfer call, a token without a limit, or a spend
//! past the remaining allowance — needs the full threshold, exactly as before.
//! Full-threshold calls never consume allowance.
//!
//! # Windows
//!
//! Spending is tracked per `(signer, token)` in instance storage as a
//! [`SpendingLimit`] whose `window_start` is the ledger timestamp rounded down
//! to a multiple of [`WINDOW_SECONDS`]. The first spend in a later window
//! starts a fresh quota, so the allowance resets when the ledger timestamp
//! advances into the next 24-hour window.
//!
//! When several sub-threshold signers attach to one call, the amount is
//! charged to *each* of them, so no signer can exceed its own quota by
//! co-signing with another.

use soroban_sdk::{
    auth::{Context, ContractContext},
    contractevent, contracttype, symbol_short, Address, Env, TryFromVal, Vec,
};

use crate::{DataKey, Error};

/// Length of one spending window: 24 hours of ledger time.
pub const WINDOW_SECONDS: u64 = 86_400;

/// A signer's spending in one token for the current window.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpendingLimit {
    /// Start timestamp of the window this record belongs to.
    pub window_start: u64,
    /// Amount already spent in that window.
    pub spent: i128,
}

/// Emitted when governance sets or clears a token's daily limit.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DailyLimitSet {
    #[topic]
    pub token: Address,
    pub limit: i128,
}

fn window_start(env: &Env) -> u64 {
    let now = env.ledger().timestamp();
    now - now % WINDOW_SECONDS
}

/// Set the daily allowance for `token`. `0` removes it, disabling
/// sub-threshold spending of that token. Requires the account's own
/// authorization, i.e. the full threshold.
pub fn set_daily_limit(env: &Env, token: Address, limit: i128) -> Result<(), Error> {
    env.current_contract_address().require_auth();
    if limit < 0 {
        return Err(Error::InvalidLimit);
    }
    let key = DataKey::DailyLimit(token.clone());
    if limit == 0 {
        env.storage().instance().remove(&key);
    } else {
        env.storage().instance().set(&key, &limit);
    }
    DailyLimitSet { token, limit }.publish(env);
    Ok(())
}

/// The daily allowance configured for `token` (`0` = none).
pub fn daily_limit(env: &Env, token: &Address) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::DailyLimit(token.clone()))
        .unwrap_or(0)
}

/// What `signer` has spent of `token` in the current window.
pub fn spent_today(env: &Env, signer: &Address, token: &Address) -> i128 {
    let record: Option<SpendingLimit> = env
        .storage()
        .instance()
        .get(&DataKey::Spending(signer.clone(), token.clone()));
    match record {
        Some(r) if r.window_start == window_start(env) => r.spent,
        _ => 0,
    }
}

/// Extract `(token, amount)` from a `transfer(from, to, amount)` context
/// spending this account's funds; `None` for anything else.
fn as_own_transfer(env: &Env, ctx: &ContractContext) -> Option<(Address, i128)> {
    if ctx.fn_name != symbol_short!("transfer") || ctx.args.len() != 3 {
        return None;
    }
    let from = Address::try_from_val(env, &ctx.args.get(0)?).ok()?;
    if from != env.current_contract_address() {
        return None;
    }
    let amount = i128::try_from_val(env, &ctx.args.get(2)?).ok()?;
    if amount < 0 {
        return None;
    }
    Some((ctx.contract.clone(), amount))
}

/// Sub-threshold authorization path, called from `__check_auth` when fewer
/// than `threshold` signers attached. Admits the call only if every context
/// is an own-funds token transfer within each delegate's remaining daily
/// allowance, and records the spend.
pub(crate) fn authorize_within_limits(
    env: &Env,
    delegates: &Vec<Address>,
    auth_contexts: &Vec<Context>,
) -> Result<(), Error> {
    if delegates.is_empty() || auth_contexts.is_empty() {
        return Err(Error::InsufficientSignatures);
    }

    // Sum the spend per token across every authorized context first, so two
    // transfers in one call cannot each pass the check on their own.
    let mut totals: soroban_sdk::Map<Address, i128> = soroban_sdk::Map::new(env);
    for ctx in auth_contexts.iter() {
        let Context::Contract(c) = ctx else {
            return Err(Error::InsufficientSignatures);
        };
        let (token, amount) = as_own_transfer(env, &c).ok_or(Error::InsufficientSignatures)?;
        let so_far = totals.get(token.clone()).unwrap_or(0);
        totals.set(
            token,
            so_far
                .checked_add(amount)
                .ok_or(Error::DailyLimitExceeded)?,
        );
    }

    let window = window_start(env);
    for (token, amount) in totals.iter() {
        let limit = daily_limit(env, &token);
        if limit == 0 {
            return Err(Error::InsufficientSignatures);
        }
        for signer in delegates.iter() {
            let spent = spent_today(env, &signer, &token);
            let next = spent.checked_add(amount).ok_or(Error::DailyLimitExceeded)?;
            if next > limit {
                return Err(Error::DailyLimitExceeded);
            }
            env.storage().instance().set(
                &DataKey::Spending(signer, token.clone()),
                &SpendingLimit {
                    window_start: window,
                    spent: next,
                },
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::WINDOW_SECONDS;
    use crate::testutils::make_auth_entry_with_nonce;
    use crate::{Error, MultisigAccount, MultisigAccountClient};
    use soroban_sdk::{
        testutils::{Address as _, Ledger as _},
        token::{StellarAssetClient, TokenClient},
        vec, Address, Env, IntoVal, Val,
    };

    const LIMIT: i128 = 1_000;

    struct Fx {
        env: Env,
        account: Address,
        token: Address,
        s1: Address,
        s2: Address,
        to: Address,
        nonce: core::cell::Cell<i64>,
    }

    /// 2-of-3 account holding 10_000 of a token with a daily limit of
    /// [`LIMIT`]. Auth mocking is switched off again once setup is done, so
    /// every later call runs the account's real `__check_auth`.
    fn setup() -> Fx {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger()
            .with_mut(|l| l.timestamp = 10 * WINDOW_SECONDS + 100);

        let (s1, s2, s3) = (
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
        );
        let account = env.register(
            MultisigAccount,
            (vec![&env, s1.clone(), s2.clone(), s3], 2u32),
        );
        let token = env
            .register_stellar_asset_contract_v2(Address::generate(&env))
            .address();
        StellarAssetClient::new(&env, &token).mint(&account, &10_000);
        MultisigAccountClient::new(&env, &account).set_daily_limit(&token, &LIMIT);
        env.set_auths(&[]);

        Fx {
            to: Address::generate(&env),
            env,
            account,
            token,
            s1,
            s2,
            nonce: core::cell::Cell::new(1),
        }
    }

    impl Fx {
        fn client(&self) -> MultisigAccountClient<'_> {
            MultisigAccountClient::new(&self.env, &self.account)
        }

        /// Transfer `amount` from the account, authorized by `signers`.
        fn transfer(&self, signers: &[Address], amount: i128) -> bool {
            let args: [Val; 3] = [
                self.account.clone().into_val(&self.env),
                self.to.clone().into_val(&self.env),
                amount.into_val(&self.env),
            ];
            let nonce = self.nonce.get();
            self.nonce.set(nonce + 1);
            let entry = make_auth_entry_with_nonce(
                &self.env,
                &self.account,
                &self.token,
                "transfer",
                &args,
                signers,
                nonce,
            );
            self.env.set_auths(&[entry]);
            TokenClient::new(&self.env, &self.token)
                .try_transfer(&self.account, &self.to, &amount)
                .is_ok()
        }

        fn received(&self) -> i128 {
            TokenClient::new(&self.env, &self.token).balance(&self.to)
        }
    }

    #[test]
    fn single_signer_spends_within_daily_limit() {
        let fx = setup();
        assert!(fx.transfer(core::slice::from_ref(&fx.s1), 400));
        assert!(fx.transfer(core::slice::from_ref(&fx.s1), 600));
        assert_eq!(fx.received(), 1_000);
        assert_eq!(fx.client().get_spent_today(&fx.s1, &fx.token), 1_000);
    }

    #[test]
    fn single_signer_is_rejected_past_the_cap() {
        let fx = setup();
        assert!(!fx.transfer(core::slice::from_ref(&fx.s1), LIMIT + 1));
        assert!(fx.transfer(core::slice::from_ref(&fx.s1), 900));
        // Cap exhausted: even a small spend now needs the full threshold.
        assert!(!fx.transfer(core::slice::from_ref(&fx.s1), 101));
        assert_eq!(fx.received(), 900);
        assert_eq!(fx.client().get_spent_today(&fx.s1, &fx.token), 900);
    }

    #[test]
    fn quotas_are_tracked_per_signer() {
        let fx = setup();
        assert!(fx.transfer(core::slice::from_ref(&fx.s1), LIMIT));
        assert!(fx.transfer(core::slice::from_ref(&fx.s2), LIMIT));
        assert_eq!(fx.client().get_spent_today(&fx.s2, &fx.token), LIMIT);
    }

    #[test]
    fn full_threshold_bypasses_the_cap_and_consumes_no_quota() {
        let fx = setup();
        assert!(fx.transfer(&[fx.s1.clone(), fx.s2.clone()], 5_000));
        assert_eq!(fx.received(), 5_000);
        assert_eq!(fx.client().get_spent_today(&fx.s1, &fx.token), 0);
    }

    #[test]
    fn quota_resets_in_the_next_window() {
        let fx = setup();
        assert!(fx.transfer(core::slice::from_ref(&fx.s1), LIMIT));
        assert!(!fx.transfer(core::slice::from_ref(&fx.s1), 1));

        // Still inside the same 24-hour window.
        fx.env
            .ledger()
            .with_mut(|l| l.timestamp = 11 * WINDOW_SECONDS - 1);
        assert!(!fx.transfer(core::slice::from_ref(&fx.s1), 1));

        // First second of the next window.
        fx.env
            .ledger()
            .with_mut(|l| l.timestamp = 11 * WINDOW_SECONDS);
        assert_eq!(fx.client().get_spent_today(&fx.s1, &fx.token), 0);
        assert!(fx.transfer(core::slice::from_ref(&fx.s1), LIMIT));
        assert_eq!(fx.received(), 2 * LIMIT);
    }

    #[test]
    fn token_without_limit_needs_full_threshold() {
        let fx = setup();
        fx.env.mock_all_auths();
        fx.client().set_daily_limit(&fx.token, &0);
        fx.env.set_auths(&[]);
        assert_eq!(fx.client().get_daily_limit(&fx.token), 0);
        assert!(!fx.transfer(core::slice::from_ref(&fx.s1), 1));
    }

    #[test]
    fn non_transfer_call_needs_full_threshold() {
        let fx = setup();
        let args: [Val; 2] = [fx.token.clone().into_val(&fx.env), 5i128.into_val(&fx.env)];
        let entry = make_auth_entry_with_nonce(
            &fx.env,
            &fx.account,
            &fx.account,
            "set_daily_limit",
            &args,
            core::slice::from_ref(&fx.s1),
            1,
        );
        fx.env.set_auths(&[entry]);
        assert!(fx.client().try_set_daily_limit(&fx.token, &5).is_err());
        assert_eq!(fx.client().get_daily_limit(&fx.token), LIMIT);
    }

    #[test]
    fn negative_limit_is_rejected() {
        let fx = setup();
        fx.env.mock_all_auths();
        assert_eq!(
            fx.client().try_set_daily_limit(&fx.token, &-1),
            Err(Ok(Error::InvalidLimit))
        );
    }

    #[test]
    #[should_panic]
    fn setting_a_limit_requires_account_auth() {
        let fx = setup();
        fx.client().set_daily_limit(&fx.token, &5);
    }
}
