//! Automated Revenue Sweeping to Yield Protocols (issue #479).
//!
//! Automatically sweep protocol fee revenue exceeding a base threshold into
//! whitelisted yield aggregators every 7 days.
//!
//! - [`SweepConfig`] sets the minimum reserve threshold that must stay liquid
//!   and the minimum elapsed duration between sweeps (defaulting to 7 days).
//! - [`is_sweep_due`] determines whether the timestamp interval has elapsed
//!   and whether liquid funds exceed the threshold.
//! - [`calculate_excess`] computes how much revenue is eligible for deployment.
//! - [`sweep_revenue`] executes the deployment using the existing strategy
//!   allocation system ([`strategies::rebalance`]), updating the last sweep
//!   timestamp and emitting [`RevenueSweptEvent`].

use soroban_sdk::{contractevent, contracttype, token, Address, Env};

use crate::{require_admin, require_initialized, strategies, DataKey, Error};

/// Default sweep interval: 7 days in seconds (7 * 24 * 3600 = 604,800).
pub const DEFAULT_SWEEP_INTERVAL_SECS: u64 = 7 * 24 * 60 * 60;

/// Storage key for sweep configuration.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SweepDataKey {
    Config,
}

/// Configuration for automated revenue sweeping.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SweepConfig {
    /// Base liquid float threshold. Revenue exceeding this is swept.
    pub threshold: i128,
    /// Minimum interval in seconds between sweeps (default 7 days).
    pub sweep_interval: u64,
    /// Unix timestamp of the last executed sweep.
    pub last_sweep: u64,
}

/// Emitted when a revenue sweep is executed.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RevenueSweptEvent {
    #[topic]
    pub timestamp: u64,
    pub swept_amount: i128,
    pub new_last_sweep: u64,
}

/// Configure automated sweeping. Callable only by admin.
pub fn set_sweep_config(env: &Env, threshold: i128, sweep_interval: u64) -> Result<(), Error> {
    require_admin(env);

    if threshold < 0 {
        return Err(Error::InvalidReserve);
    }
    if sweep_interval == 0 {
        return Err(Error::InvalidSchedule);
    }

    let last_sweep = get_sweep_config(env)
        .map(|c| c.last_sweep)
        .unwrap_or_else(|| env.ledger().timestamp());

    let config = SweepConfig {
        threshold,
        sweep_interval,
        last_sweep,
    };

    env.storage().instance().set(&SweepDataKey::Config, &config);
    Ok(())
}

/// Read the current sweep configuration, or `None` if unconfigured.
pub fn get_sweep_config(env: &Env) -> Option<SweepConfig> {
    env.storage().instance().get(&SweepDataKey::Config)
}

/// Calculate current excess revenue above the configured base threshold.
pub fn calculate_excess(env: &Env) -> Result<i128, Error> {
    let config = get_sweep_config(env).ok_or(Error::NotInitialized)?;
    let token_addr: Address = env
        .storage()
        .instance()
        .get(&DataKey::Token)
        .ok_or(Error::NotInitialized)?;

    let balance = token::Client::new(env, &token_addr).balance(&env.current_contract_address());
    let excess = balance.saturating_sub(config.threshold);
    Ok(if excess > 0 { excess } else { 0 })
}

/// Check whether an automated sweep is currently due.
pub fn is_sweep_due(env: &Env) -> Result<bool, Error> {
    let config = match get_sweep_config(env) {
        Some(c) => c,
        None => return Ok(false),
    };

    let now = env.ledger().timestamp();
    let due_time = now >= config.last_sweep.saturating_add(config.sweep_interval);
    let has_excess = calculate_excess(env)? > 0;

    Ok(due_time && has_excess)
}

/// Execute revenue sweeping into whitelisted yield aggregators.
///
/// Cron-compatible entrypoint: callable by anyone when a sweep is due.
pub fn sweep_revenue(env: &Env) -> Result<i128, Error> {
    require_initialized(env)?;

    let mut config = get_sweep_config(env).ok_or(Error::NotInitialized)?;
    let now = env.ledger().timestamp();

    if now < config.last_sweep.saturating_add(config.sweep_interval) {
        return Err(Error::NothingToDeploy);
    }

    let excess = calculate_excess(env)?;
    if excess <= 0 {
        return Err(Error::NothingToDeploy);
    }

    // Deploy excess into whitelisted yield protocols via the existing strategy engine
    strategies::with_lock(env, || strategies::rebalance(env))?;

    config.last_sweep = now;
    env.storage().instance().set(&SweepDataKey::Config, &config);

    RevenueSweptEvent {
        timestamp: now,
        swept_amount: excess,
        new_last_sweep: now,
    }
    .publish(env);

    Ok(excess)
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        token::StellarAssetClient,
    };

    struct TestHarness {
        env: Env,
        contract_id: Address,
        _admin: Address,
        token: Address,
    }

    fn setup() -> TestHarness {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let sac = env.register_stellar_asset_contract_v2(token_admin);
        let token = sac.address();

        let contract_id = env.register(crate::Treasury, (admin.clone(), token.clone()));

        TestHarness {
            env,
            contract_id,
            _admin: admin,
            token,
        }
    }

    #[test]
    fn test_sweep_configuration_and_due_check() {
        let h = setup();
        StellarAssetClient::new(&h.env, &h.token).mint(&h.contract_id, &50_000);

        h.env.as_contract(&h.contract_id, || {
            assert!(get_sweep_config(&h.env).is_none());
            assert!(!is_sweep_due(&h.env).unwrap());

            // Set config: threshold 10_000, 7-day interval
            set_sweep_config(&h.env, 10_000, DEFAULT_SWEEP_INTERVAL_SECS).unwrap();
            let config = get_sweep_config(&h.env).unwrap();
            assert_eq!(config.threshold, 10_000);
            assert_eq!(config.sweep_interval, DEFAULT_SWEEP_INTERVAL_SECS);

            assert_eq!(calculate_excess(&h.env).unwrap(), 40_000);

            // Not due yet (0 seconds elapsed)
            assert!(!is_sweep_due(&h.env).unwrap());

            // Fast forward 7 days
            h.env
                .ledger()
                .set_timestamp(h.env.ledger().timestamp() + DEFAULT_SWEEP_INTERVAL_SECS);
            assert!(is_sweep_due(&h.env).unwrap());
        });
    }

    #[test]
    fn test_sweep_revenue_execution_and_timestamp_update() {
        let h = setup();
        StellarAssetClient::new(&h.env, &h.token).mint(&h.contract_id, &60_000);

        h.env.as_contract(&h.contract_id, || {
            set_sweep_config(&h.env, 20_000, DEFAULT_SWEEP_INTERVAL_SECS).unwrap();

            // Advance time by 7 days
            let sweep_time = h.env.ledger().timestamp() + DEFAULT_SWEEP_INTERVAL_SECS;
            h.env.ledger().set_timestamp(sweep_time);

            let swept = sweep_revenue(&h.env).unwrap();
            assert_eq!(swept, 40_000);

            let config = get_sweep_config(&h.env).unwrap();
            assert_eq!(config.last_sweep, sweep_time);

            // Calling immediately again fails because interval has not elapsed
            let err = sweep_revenue(&h.env);
            assert_eq!(err, Err(Error::NothingToDeploy));
        });
    }
}
