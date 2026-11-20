extern crate std;

use soroban_sdk::{
    contract, contractimpl,
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    vec, Address, Env,
};
use treasury::strategies::AllocationConfig;
use treasury::{Error, Treasury, TreasuryClient};

#[contract]
pub struct MockUnderdeliveringStrategy;

#[contractimpl]
impl MockUnderdeliveringStrategy {
    pub fn __constructor(env: Env, token: Address, treasury: Address) {
        env.storage().instance().set(&0u32, &token);
        env.storage().instance().set(&1u32, &treasury);
        env.storage().instance().set(&2u32, &0i128);
    }

    pub fn deposit(env: Env, amount: i128) -> Result<(), Error> {
        let total: i128 = env.storage().instance().get(&2u32).unwrap_or(0);
        env.storage().instance().set(&2u32, &(total + amount));
        Ok(())
    }

    pub fn withdraw(env: Env, principal: i128) -> Result<(i128, i128), Error> {
        let token_addr: Address = env.storage().instance().get(&0u32).unwrap();
        let treasury: Address = env.storage().instance().get(&1u32).unwrap();

        // Under-deliver: transfer only 1 token back to treasury, far short of principal
        TokenClient::new(&env, &token_addr).transfer(
            &env.current_contract_address(),
            &treasury,
            &1i128,
        );

        // Falsely report full principal returned
        Ok((principal, 0i128))
    }
}

#[test]
fn recall_strategy_rejects_under_delivering_strategy_and_preserves_allocations() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    let treasury = env.register(Treasury, (admin.clone(), token.clone()));
    let client = TreasuryClient::new(&env, &treasury);

    // Fund treasury float
    StellarAssetClient::new(&env, &token).mint(&treasury, &100_000);

    // Deploy mock under-delivering strategy
    let strategy = env.register(
        MockUnderdeliveringStrategy,
        (token.clone(), treasury.clone()),
    );
    StellarAssetClient::new(&env, &token).mint(&strategy, &100_000);

    // Whitelist and allocate 50% to strategy (reserve 50% liquid)
    client.whitelist_strategy(&strategy);
    client.set_reserve_bps(&5_000); // 50% liquid
    let configs = vec![
        &env,
        AllocationConfig {
            strategy: strategy.clone(),
            weight_bps: 10_000,
        },
    ];
    client.set_allocations(&configs);

    // Rebalance portfolio: deploys 50% of 100_000 = 50_000 to strategy
    let deployed = client.rebalance_portfolio();
    assert_eq!(deployed, 50_000);

    let alloc_before = client.get_allocation(&strategy).unwrap();
    assert_eq!(alloc_before.deployed, 50_000);
    assert_eq!(alloc_before.yield_earned, 0);

    // Attempt to recall 25_000 from strategy
    let recall_attempt = client.try_recall_strategy(&strategy, &25_000);
    assert_eq!(recall_attempt, Err(Ok(Error::StrategyUnderpaid)));

    // Allocation must remain intact and uncorrupted
    let alloc_after = client.get_allocation(&strategy).unwrap();
    assert_eq!(alloc_after.deployed, 50_000);
    assert_eq!(alloc_after.yield_earned, 0);
}
