//! Boundary tests for `strategy::recall_principal`'s checked arithmetic
//! (issue #723).
//!
//! `recall_principal` sums two strategy-reported values it does not control:
//! `principal_returned + yield_returned` for the balance cross-check, and
//! `harvested + yield_returned` for the `HarvestedYield` bookkeeping. An
//! untrusted strategy can return any `i128` pair, so either sum can overflow no
//! matter what the vault itself holds. The tests drive each sum to the `i128`
//! ceiling and assert the typed `Error::MathOverflow` arm, which only exists
//! because the computation is checked — a bare `+` would abort the host call.

use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    Address, Env,
};

use crate::test_helpers::vault_init;
use crate::{DataKey, Error, RefundVault, RefundVaultClient};

// ── Mock: a strategy that reports (and pays) exactly what the test asks ─────

#[contracttype]
enum BoundaryKey {
    Token,
    Vault,
    PrincipalReturned,
    YieldReturned,
    Transferred,
}

#[contract]
pub struct BoundaryStrategy;

#[contractimpl]
impl BoundaryStrategy {
    pub fn initialize(env: Env, token: Address, vault: Address) {
        env.storage().instance().set(&BoundaryKey::Token, &token);
        env.storage().instance().set(&BoundaryKey::Vault, &vault);
    }

    /// Set the `(principal_returned, yield_returned)` pair reported by the
    /// next `withdraw`, and how many tokens it actually pays back.
    pub fn configure(env: Env, principal_returned: i128, yield_returned: i128, transferred: i128) {
        env.storage()
            .instance()
            .set(&BoundaryKey::PrincipalReturned, &principal_returned);
        env.storage()
            .instance()
            .set(&BoundaryKey::YieldReturned, &yield_returned);
        env.storage()
            .instance()
            .set(&BoundaryKey::Transferred, &transferred);
    }

    pub fn deposit(_env: Env, _amount: i128) -> Result<(), Error> {
        Ok(())
    }

    pub fn withdraw(env: Env, _principal: i128) -> Result<(i128, i128), Error> {
        let transferred: i128 = env
            .storage()
            .instance()
            .get(&BoundaryKey::Transferred)
            .unwrap_or(0);
        if transferred > 0 {
            let token: Address = env.storage().instance().get(&BoundaryKey::Token).unwrap();
            let vault: Address = env.storage().instance().get(&BoundaryKey::Vault).unwrap();
            TokenClient::new(&env, &token).transfer(
                &env.current_contract_address(),
                &vault,
                &transferred,
            );
        }

        let principal_returned: i128 = env
            .storage()
            .instance()
            .get(&BoundaryKey::PrincipalReturned)
            .unwrap_or(0);
        let yield_returned: i128 = env
            .storage()
            .instance()
            .get(&BoundaryKey::YieldReturned)
            .unwrap_or(0);
        Ok((principal_returned, yield_returned))
    }

    pub fn harvest(_env: Env) -> Result<i128, Error> {
        Ok(0)
    }

    pub fn accrued_yield(_env: Env) -> i128 {
        0
    }
}

const DEPLOYED: i128 = 10;

/// Deploy [`DEPLOYED`] units into a [`BoundaryStrategy`] configured to report
/// `(principal_returned, yield_returned)` and pay back `transferred`.
fn setup(
    principal_returned: i128,
    yield_returned: i128,
    transferred: i128,
) -> (Env, RefundVaultClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &1_000);

    let vault_id = env.register(RefundVault, (vault_init(&env, &merchant, &token, 17_280),));
    let vault = RefundVaultClient::new(&env, &vault_id);

    let strategy = env.register(BoundaryStrategy, ());
    let strategy_client = BoundaryStrategyClient::new(&env, &strategy);
    strategy_client.initialize(&token, &vault_id);
    strategy_client.configure(&principal_returned, &yield_returned, &transferred);

    vault.approve_yield_strategy(&strategy);
    vault.set_yield_strategy(&strategy);
    vault.deposit(&merchant, &1_000, &None);
    vault.deploy_to_yield(&DEPLOYED);

    (env, vault, vault_id)
}

#[test]
fn recall_principal_overflowing_return_sum_returns_math_overflow() {
    // One unit of principal against `i128::MAX` of reported yield: the
    // cross-check sum `principal_returned + yield_returned` is not
    // representable. No tokens need to move for this path to be taken.
    let (_env, vault, _vault_id) = setup(1, i128::MAX, 0);

    assert_eq!(
        vault.try_withdraw_from_yield(&1),
        Err(Ok(Error::MathOverflow))
    );
}

#[test]
fn recall_principal_overflowing_harvested_sum_returns_math_overflow() {
    // `(1, 1)` fits in `i128` and the strategy really pays the 2 units the
    // balance cross-check expects, so the first sum passes; adding that 1 unit
    // of yield to an already-maximal `HarvestedYield` total is what overflows.
    let (env, vault, vault_id) = setup(1, 1, 2);

    env.as_contract(&vault_id, || {
        env.storage()
            .persistent()
            .set(&DataKey::HarvestedYield, &i128::MAX);
    });

    assert_eq!(
        vault.try_withdraw_from_yield(&1),
        Err(Ok(Error::MathOverflow))
    );

    // The typed failure reverted the invocation, so the bookkeeping it had
    // already started writing is untouched.
    env.as_contract(&vault_id, || {
        let harvested: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::HarvestedYield)
            .unwrap();
        assert_eq!(harvested, i128::MAX);
        let deployed: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::DeployedPrincipal)
            .unwrap();
        assert_eq!(deployed, DEPLOYED);
    });
}
