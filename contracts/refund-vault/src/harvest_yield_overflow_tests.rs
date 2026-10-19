//! Boundary test for `harvest_yield`'s checked running total (issue #720).
//!
//! `harvest_yield` accumulated the newly harvested amount into the stored
//! `HarvestedYield` total with a bare `+`. With `[profile.release]` setting
//! `overflow-checks = true` and `panic = "abort"`, a non-representable sum
//! aborted the host call with a `WasmRuntimeError` instead of answering with a
//! typed error. The addition now routes through
//! `accensa_common::math::add_amounts`, whose `MathError::Overflow` maps to
//! `Error::MathOverflow`.
//!
//! The test pins the stored total at `i128::MAX` and has the strategy report
//! exactly one more unit of yield: the sum is unrepresentable, so the call
//! returns the typed variant rather than trapping.

use soroban_sdk::{contract, contractimpl, contracttype, testutils::Address as _, Address, Env};

use crate::test_helpers::vault_init;
use crate::{DataKey, Error, RefundVault, RefundVaultClient};

#[contracttype]
enum StrategyKey {
    Yield,
}

/// Minimal [`YieldStrategy`](crate::YieldStrategy) whose `harvest` reports a
/// configured amount. No tokens move: `harvest_yield`'s sum is what this test
/// exercises, and the vault does not check the float on that path.
#[contract]
pub struct HarvestBoundaryStrategy;

#[contractimpl]
impl HarvestBoundaryStrategy {
    pub fn set_yield(env: Env, amount: i128) {
        env.storage().instance().set(&StrategyKey::Yield, &amount);
    }

    pub fn deposit(_env: Env, _amount: i128) -> Result<(), Error> {
        Ok(())
    }

    pub fn withdraw(_env: Env, _principal: i128) -> Result<(i128, i128), Error> {
        Ok((0, 0))
    }

    pub fn harvest(env: Env) -> Result<i128, Error> {
        Ok(env
            .storage()
            .instance()
            .get(&StrategyKey::Yield)
            .unwrap_or(0))
    }

    pub fn total_balance(_env: Env) -> i128 {
        0
    }

    pub fn accrued_yield(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&StrategyKey::Yield)
            .unwrap_or(0)
    }
}

fn setup(yield_amount: i128) -> (Env, RefundVaultClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();

    let vault_id = env.register(RefundVault, (vault_init(&env, &merchant, &token, 17_280),));
    let vault = RefundVaultClient::new(&env, &vault_id);

    let strategy = env.register(HarvestBoundaryStrategy, ());
    HarvestBoundaryStrategyClient::new(&env, &strategy).set_yield(&yield_amount);

    vault.approve_yield_strategy(&strategy);
    vault.set_yield_strategy(&strategy);

    (env, vault, vault_id)
}

#[test]
fn harvest_yield_overflowing_running_total_returns_math_overflow() {
    // One unit of new yield on top of an already-maximal running total is not
    // representable in `i128`: the sum must fail with the typed variant, not
    // abort the host call.
    let (env, vault, vault_id) = setup(1);

    env.as_contract(&vault_id, || {
        env.storage()
            .persistent()
            .set(&DataKey::HarvestedYield, &i128::MAX);
    });

    assert_eq!(vault.try_harvest_yield(), Err(Ok(Error::MathOverflow)));

    // The typed failure reverted the invocation, so the stored total and the
    // nonce are untouched.
    env.as_contract(&vault_id, || {
        let harvested: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::HarvestedYield)
            .unwrap();
        assert_eq!(harvested, i128::MAX);
    });
}
