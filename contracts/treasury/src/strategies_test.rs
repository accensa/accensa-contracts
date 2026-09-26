//! Diversified stablecoin yield strategy tests (issue #466).
//!
//! Covers the allocation math (exact splits, rounding dust, malformed weight
//! sets), the whitelist/allocation guards, the rotation performed by
//! `rebalance_portfolio`, and yield withdrawal — including the two ways a
//! hostile strategy can misbehave (under-paying on recall, re-entering the
//! treasury mid-rotation).

#![cfg(test)]
#![allow(unused_imports, unused_variables, dead_code)]

extern crate std;

use crate::strategies::{split_by_weights, AllocationConfig, MAX_STRATEGIES, TOTAL_WEIGHT_BPS};
use crate::{Error, Treasury, TreasuryClient};
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype,
    testutils::{Address as _, Ledger as _},
    token::{StellarAssetClient, TokenClient},
    vec, Address, Env, Vec,
};

// ── Mock strategy contracts ───────────────────────────────────────────────

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StrategyError {
    Unauthorized = 1,
    InsufficientBalance = 2,
    NothingToWithdraw = 3,
    /// Signals a strategy that reports more than it transfers.
    Shortchange = 4,
}

#[contracttype]
#[derive(Clone)]
pub enum StrategyDataKey {
    Token,
    Treasury,
    TotalDeposited,
    YieldAccrued,
}

/// A well-behaved Aave-style strategy: it custodies the tokens the treasury
/// sends, accrues yield on demand, and returns principal plus the proportional
/// share of that yield on withdrawal.
#[contract]
pub struct MockStrategy;

#[contractimpl]
impl MockStrategy {
    /// Register the strategy for `treasury`, holding `token` as its asset.
    pub fn __constructor(env: Env, token: Address, treasury: Address) {
        env.storage()
            .instance()
            .set(&StrategyDataKey::Token, &token);
        env.storage()
            .instance()
            .set(&StrategyDataKey::Treasury, &treasury);
        env.storage()
            .instance()
            .set(&StrategyDataKey::TotalDeposited, &0i128);
        env.storage()
            .instance()
            .set(&StrategyDataKey::YieldAccrued, &0i128);
    }

    /// Simulate interest accruing (the treasury itself cannot call this — the
    /// mock's control surface stands in for the protocol's own dynamics).
    ///
    /// This moves only the *accounting*. The matching tokens are minted into
    /// the strategy's custody by the test helper [`accrue`], exactly as a
    /// lending pool already holds the interest its depositors will later
    /// claim — so a `withdraw` only ever moves tokens the strategy really
    /// holds, with no mint inside the call.
    pub fn simulate_yield(env: Env, amount: i128) {
        let current: i128 = env
            .storage()
            .instance()
            .get(&StrategyDataKey::YieldAccrued)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&StrategyDataKey::YieldAccrued, &(current + amount));
    }

    pub fn deposited(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&StrategyDataKey::TotalDeposited)
            .unwrap_or(0)
    }

    pub fn deposit(env: Env, amount: i128) -> Result<(), StrategyError> {
        if amount <= 0 {
            return Err(StrategyError::InsufficientBalance);
        }
        let total: i128 = env
            .storage()
            .instance()
            .get(&StrategyDataKey::TotalDeposited)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&StrategyDataKey::TotalDeposited, &(total + amount));
        Ok(())
    }

    pub fn withdraw(env: Env, principal: i128) -> Result<(i128, i128), StrategyError> {
        let total: i128 = env
            .storage()
            .instance()
            .get(&StrategyDataKey::TotalDeposited)
            .unwrap_or(0);
        if principal <= 0 || principal > total {
            return Err(StrategyError::NothingToWithdraw);
        }
        let accrued: i128 = env
            .storage()
            .instance()
            .get(&StrategyDataKey::YieldAccrued)
            .unwrap_or(0);
        let yield_portion = accrued * principal / total;

        env.storage()
            .instance()
            .set(&StrategyDataKey::TotalDeposited, &(total - principal));
        env.storage()
            .instance()
            .set(&StrategyDataKey::YieldAccrued, &(accrued - yield_portion));

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&StrategyDataKey::Token)
            .unwrap();
        let treasury: Address = env
            .storage()
            .instance()
            .get(&StrategyDataKey::Treasury)
            .unwrap();
        // Principal and the proportional share of accrued interest both come
        // out of the strategy's own custody.
        let client = TokenClient::new(&env, &token_addr);
        client.transfer(
            &env.current_contract_address(),
            &treasury,
            &(principal + yield_portion),
        );

        Ok((principal, yield_portion))
    }

    pub fn total_balance(env: Env) -> i128 {
        let token_addr: Address = env
            .storage()
            .instance()
            .get(&StrategyDataKey::Token)
            .unwrap();
        TokenClient::new(&env, &token_addr).balance(&env.current_contract_address())
    }

    pub fn accrued_yield(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&StrategyDataKey::YieldAccrued)
            .unwrap_or(0)
    }
}

/// A strategy that reports a larger return than it actually sends — the
/// bookkeeping must reject it rather than credit phantom principal.
#[contract]
pub struct ShortchangingStrategy;

#[contractimpl]
impl ShortchangingStrategy {
    pub fn __constructor(env: Env, token: Address, treasury: Address) {
        env.storage()
            .instance()
            .set(&StrategyDataKey::Token, &token);
        env.storage()
            .instance()
            .set(&StrategyDataKey::Treasury, &treasury);
    }

    pub fn deposit(env: Env, amount: i128) -> Result<(), StrategyError> {
        let total: i128 = env
            .storage()
            .instance()
            .get(&StrategyDataKey::TotalDeposited)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&StrategyDataKey::TotalDeposited, &(total + amount));
        Ok(())
    }

    /// Sends back only half of what it claims to have returned.
    pub fn withdraw(env: Env, principal: i128) -> Result<(i128, i128), StrategyError> {
        let token_addr: Address = env
            .storage()
            .instance()
            .get(&StrategyDataKey::Token)
            .unwrap();
        let treasury: Address = env
            .storage()
            .instance()
            .get(&StrategyDataKey::Treasury)
            .unwrap();
        let client = TokenClient::new(&env, &token_addr);
        client.transfer(&env.current_contract_address(), &treasury, &(principal / 2));
        Ok((principal, 0))
    }

    pub fn total_balance(env: Env) -> i128 {
        0
    }

    pub fn accrued_yield(env: Env) -> i128 {
        0
    }
}

// ── Fixtures ──────────────────────────────────────────────────────────────

const SUPPLY: i128 = 100_000_000;
/// Ledger timestamp the strategies tests start from.
const SUPPLY_TS: u64 = 1_700_000_000;

struct Ctx {
    env: Env,
    client: TreasuryClient<'static>,
    token: Address,
    treasury: Address,
    admin: Address,
    aave: Address,
    compound: Address,
}

fn setup() -> Ctx {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| li.timestamp = SUPPLY_TS);

    let admin = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let treasury = env.register(Treasury, (admin.clone(), token.clone()));
    let client = TreasuryClient::new(&env, &treasury);
    StellarAssetClient::new(&env, &token).mint(&treasury, &SUPPLY);

    let aave = env.register(MockStrategy, (token.clone(), treasury.clone()));
    let compound = env.register(MockStrategy, (token.clone(), treasury.clone()));

    Ctx {
        env,
        client,
        token,
        treasury,
        admin,
        aave,
        compound,
    }
}

fn balance(ctx: &Ctx, who: &Address) -> i128 {
    TokenClient::new(&ctx.env, &ctx.token).balance(who)
}

fn set_time(ctx: &Ctx, timestamp: u64) {
    ctx.env.ledger().with_mut(|li| li.timestamp = timestamp);
}

/// Make `strategy` earn `amount` of interest: the tokens are minted into its
/// own custody and its accrual accounting is moved to match, so a later
/// `withdraw` pays out of real balance.
fn accrue(env: &Env, token: &Address, strategy: &Address, amount: i128) {
    StellarAssetClient::new(env, token).mint(strategy, &amount);
    MockStrategyClient::new(env, strategy).simulate_yield(&amount);
}

/// Helper: whitelist both mock strategies and set a 50/50 allocation.
fn configure_two_way(ctx: &Ctx) {
    ctx.client.whitelist_strategy(&ctx.aave);
    ctx.client.whitelist_strategy(&ctx.compound);
    ctx.client.set_allocations(&vec![
        &ctx.env,
        AllocationConfig {
            strategy: ctx.aave.clone(),
            weight_bps: 5_000,
        },
        AllocationConfig {
            strategy: ctx.compound.clone(),
            weight_bps: 5_000,
        },
    ]);
    ctx.client.set_reserve_bps(&0);
}

// ── Allocation math ───────────────────────────────────────────────────────

#[test]
fn weights_split_a_balance_exactly() {
    let env = Env::default();
    let (a, b, c) = (
        Address::generate(&env),
        Address::generate(&env),
        Address::generate(&env),
    );
    let weights = vec![
        &env,
        (a.clone(), 6_000),
        (b.clone(), 3_000),
        (c.clone(), 1_000),
    ];

    let shares = split_by_weights(&env, 1_000_000, &weights).unwrap();
    assert_eq!(shares, vec![&env, (a, 600_000), (b, 300_000), (c, 100_000)]);
}

#[test]
fn rounding_dust_lands_on_the_largest_weight() {
    let env = Env::default();
    let (a, b) = (Address::generate(&env), Address::generate(&env));
    // 1/3 + 1/3 + 1/3 does not divide evenly: 1,000 units into thirds leave a
    // remainder of 1, which must go to one strategy rather than be stranded.
    let weights = vec![
        &env,
        (a.clone(), 3_334),
        (a.clone(), 3_333),
        (b.clone(), 3_333),
    ];
    let shares = split_by_weights(&env, 1_000, &weights).unwrap();
    let total: i128 = shares.iter().map(|(_, amount)| amount).sum();
    assert_eq!(total, 1_000, "every unit of the balance must be allocated");
    assert_eq!(shares.get(0).unwrap().1, 334);
}

#[test]
fn a_zero_balance_allocates_nothing() {
    let env = Env::default();
    let a = Address::generate(&env);
    let weights = vec![&env, (a.clone(), 10_000)];
    let shares = split_by_weights(&env, 0, &weights).unwrap();
    assert_eq!(shares, vec![&env, (a, 0)]);
}

#[test]
fn malformed_weight_sets_are_rejected() {
    let env = Env::default();
    let (a, b) = (Address::generate(&env), Address::generate(&env));

    // Empty.
    assert_eq!(
        split_by_weights(&env, 1_000, &vec![&env]),
        Err(Error::InvalidAllocations)
    );
    // Does not sum to 100%.
    assert_eq!(
        split_by_weights(&env, 1_000, &vec![&env, (a.clone(), 5_000)]),
        Err(Error::InvalidAllocations)
    );
    // Over-allocated.
    assert_eq!(
        split_by_weights(
            &env,
            1_000,
            &vec![&env, (a.clone(), 6_000), (b.clone(), 6_000)]
        ),
        Err(Error::InvalidAllocations)
    );
    // A zero weight would strand a strategy's share.
    assert_eq!(
        split_by_weights(
            &env,
            1_000,
            &vec![&env, (a.clone(), 0), (b.clone(), 10_000)]
        ),
        Err(Error::InvalidAllocations)
    );
    // More strategies than the cap allows.
    let mut many: Vec<(Address, u32)> = Vec::new(&env);
    for _ in 0..=MAX_STRATEGIES {
        many.push_back((Address::generate(&env), 0));
    }
    assert_eq!(
        split_by_weights(&env, 1_000, &many),
        Err(Error::InvalidAllocations)
    );
}

// ── Whitelist & allocation guards ─────────────────────────────────────────

#[test]
fn only_whitelisted_strategies_can_be_allocated() {
    let ctx = setup();

    // Not whitelisted yet.
    assert_eq!(
        ctx.client.try_set_allocations(&vec![
            &ctx.env,
            AllocationConfig {
                strategy: ctx.aave.clone(),
                weight_bps: 10_000,
            }
        ]),
        Err(Ok(Error::StrategyNotWhitelisted))
    );

    ctx.client.whitelist_strategy(&ctx.aave);
    assert!(ctx.client.is_strategy_whitelisted(&ctx.aave));
    assert_eq!(
        ctx.client
            .try_set_allocations(&vec![
                &ctx.env,
                AllocationConfig {
                    strategy: ctx.aave.clone(),
                    weight_bps: 10_000,
                }
            ])
            .is_ok(),
        true
    );
}

#[test]
fn a_strategy_cannot_be_whitelisted_twice() {
    let ctx = setup();
    ctx.client.whitelist_strategy(&ctx.aave);
    assert_eq!(
        ctx.client.try_whitelist_strategy(&ctx.aave),
        Err(Ok(Error::StrategyAlreadyWhitelisted))
    );
}

#[test]
fn allocations_must_sum_to_one_hundred_percent() {
    let ctx = setup();
    ctx.client.whitelist_strategy(&ctx.aave);
    ctx.client.whitelist_strategy(&ctx.compound);

    assert_eq!(
        ctx.client.try_set_allocations(&vec![
            &ctx.env,
            AllocationConfig {
                strategy: ctx.aave.clone(),
                weight_bps: 5_000,
            },
            AllocationConfig {
                strategy: ctx.compound.clone(),
                weight_bps: 4_000,
            }
        ]),
        Err(Ok(Error::InvalidAllocations))
    );

    // A rejected set must leave the previous (empty) state untouched.
    assert!(ctx.client.get_allocations().is_empty());
}

#[test]
fn changing_weights_preserves_deployed_bookkeeping() {
    let ctx = setup();
    configure_two_way(&ctx);
    assert_eq!(ctx.client.rebalance_portfolio(), SUPPLY);

    ctx.client.set_allocations(&vec![
        &ctx.env,
        AllocationConfig {
            strategy: ctx.aave.clone(),
            weight_bps: 9_000,
        },
        AllocationConfig {
            strategy: ctx.compound.clone(),
            weight_bps: 1_000,
        },
    ]);

    let aave = ctx.client.get_allocation(&ctx.aave).unwrap();
    assert_eq!(aave.weight_bps, 9_000);
    assert_eq!(aave.deployed, SUPPLY / 2, "weights changed, funds did not");
    assert_eq!(ctx.client.get_deployed_total(), SUPPLY);
}

#[test]
fn only_the_admin_can_touch_the_portfolio() {
    let ctx = setup();
    ctx.client.whitelist_strategy(&ctx.aave);

    ctx.env.set_auths(&[]);
    assert!(ctx.client.try_whitelist_strategy(&ctx.aave).is_err());
    assert!(ctx
        .client
        .try_set_allocations(&vec![
            &ctx.env,
            AllocationConfig {
                strategy: ctx.aave.clone(),
                weight_bps: 10_000,
            }
        ])
        .is_err());
    assert!(ctx.client.try_rebalance_portfolio().is_err());
    assert!(ctx.client.try_recall_strategy(&ctx.aave, &1).is_err());
}

// ── Rebalancing ───────────────────────────────────────────────────────────

#[test]
fn rebalance_deploys_the_balance_across_the_weights() {
    let ctx = setup();
    ctx.client.whitelist_strategy(&ctx.aave);
    ctx.client.whitelist_strategy(&ctx.compound);
    ctx.client.set_allocations(&vec![
        &ctx.env,
        AllocationConfig {
            strategy: ctx.aave.clone(),
            weight_bps: 7_500,
        },
        AllocationConfig {
            strategy: ctx.compound.clone(),
            weight_bps: 2_500,
        },
    ]);
    ctx.client.set_reserve_bps(&0);

    let deployed = ctx.client.rebalance_portfolio();
    assert_eq!(deployed, SUPPLY);
    assert_eq!(
        ctx.client.get_allocation(&ctx.aave).unwrap().deployed,
        75_000_000
    );
    assert_eq!(
        ctx.client.get_allocation(&ctx.compound).unwrap().deployed,
        25_000_000
    );
    assert_eq!(ctx.client.get_deployed_total(), SUPPLY);
    assert_eq!(balance(&ctx, &ctx.treasury), 0);
    assert_eq!(balance(&ctx, &ctx.aave), 75_000_000);
    assert_eq!(balance(&ctx, &ctx.compound), 25_000_000);
}

#[test]
fn the_liquid_reserve_is_never_deployed() {
    let ctx = setup();
    ctx.client.whitelist_strategy(&ctx.aave);
    ctx.client.set_allocations(&vec![
        &ctx.env,
        AllocationConfig {
            strategy: ctx.aave.clone(),
            weight_bps: 10_000,
        },
    ]);
    // Keep 30% liquid.
    ctx.client.set_reserve_bps(&3_000);

    let deployed = ctx.client.rebalance_portfolio();
    assert_eq!(deployed, SUPPLY - SUPPLY * 3_000 / 10_000);
    assert_eq!(balance(&ctx, &ctx.treasury), SUPPLY - deployed);
    assert_eq!(balance(&ctx, &ctx.aave), deployed);
}

#[test]
fn a_treasury_defaults_to_fully_liquid() {
    let ctx = setup();
    ctx.client.whitelist_strategy(&ctx.aave);
    ctx.client.set_allocations(&vec![
        &ctx.env,
        AllocationConfig {
            strategy: ctx.aave.clone(),
            weight_bps: 10_000,
        },
    ]);

    // No reserve policy set: nothing may leave the treasury.
    assert_eq!(ctx.client.get_reserve_bps(), TOTAL_WEIGHT_BPS);
    assert_eq!(ctx.client.rebalance_portfolio(), 0);
    assert_eq!(balance(&ctx, &ctx.treasury), SUPPLY);
}

#[test]
fn rebalancing_rotates_onto_the_new_weights() {
    let ctx = setup();
    configure_two_way(&ctx);
    ctx.client.rebalance_portfolio();

    // Governance flips the split to 20/80 and rebalances.
    ctx.client.set_allocations(&vec![
        &ctx.env,
        AllocationConfig {
            strategy: ctx.aave.clone(),
            weight_bps: 2_000,
        },
        AllocationConfig {
            strategy: ctx.compound.clone(),
            weight_bps: 8_000,
        },
    ]);
    let deployed = ctx.client.rebalance_portfolio();

    assert_eq!(deployed, SUPPLY);
    assert_eq!(
        ctx.client.get_allocation(&ctx.aave).unwrap().deployed,
        20_000_000
    );
    assert_eq!(
        ctx.client.get_allocation(&ctx.compound).unwrap().deployed,
        80_000_000
    );
    // Both strategies' token balances match their new positions: the recall
    // and the redeploy both settled.
    assert_eq!(balance(&ctx, &ctx.aave), 20_000_000);
    assert_eq!(balance(&ctx, &ctx.compound), 80_000_000);
    assert_eq!(balance(&ctx, &ctx.treasury), 0);
}

#[test]
fn a_reserve_breach_brings_the_whole_portfolio_home() {
    let ctx = setup();
    configure_two_way(&ctx);
    ctx.client.rebalance_portfolio();
    assert_eq!(balance(&ctx, &ctx.treasury), 0);

    // Governance raises the reserve to 100% and rebalances: the emergency
    // exit is the same code path.
    ctx.client.set_reserve_bps(&10_000);
    assert_eq!(ctx.client.rebalance_portfolio(), 0);
    assert_eq!(ctx.client.get_deployed_total(), 0);
    assert_eq!(balance(&ctx, &ctx.treasury), SUPPLY);
    assert_eq!(balance(&ctx, &ctx.aave), 0);
    assert_eq!(balance(&ctx, &ctx.compound), 0);
}

#[test]
fn rebalancing_without_allocations_is_a_no_op() {
    let ctx = setup();
    assert_eq!(ctx.client.rebalance_portfolio(), 0);
    assert_eq!(balance(&ctx, &ctx.treasury), SUPPLY);
}

#[test]
fn an_invalid_reserve_is_rejected() {
    let ctx = setup();
    assert_eq!(
        ctx.client.try_set_reserve_bps(&10_001),
        Err(Ok(Error::InvalidReserve))
    );
}

// ── Yield withdrawal ──────────────────────────────────────────────────────

#[test]
fn recalling_principal_also_withdraws_the_yield() {
    let ctx = setup();
    ctx.client.whitelist_strategy(&ctx.aave);
    ctx.client.set_allocations(&vec![
        &ctx.env,
        AllocationConfig {
            strategy: ctx.aave.clone(),
            weight_bps: 10_000,
        },
    ]);
    ctx.client.set_reserve_bps(&0);
    ctx.client.rebalance_portfolio();

    // The strategy earns 10% interest.
    accrue(&ctx.env, &ctx.token, &ctx.aave, 10_000_000);

    // Recall half the principal: half the yield comes back with it.
    let (principal, yield_amount) = ctx.client.recall_strategy(&ctx.aave, &50_000_000);
    assert_eq!(principal, 50_000_000);
    assert_eq!(yield_amount, 5_000_000);

    let alloc = ctx.client.get_allocation(&ctx.aave).unwrap();
    assert_eq!(alloc.deployed, 50_000_000);
    assert_eq!(alloc.yield_earned, 5_000_000);
    assert_eq!(ctx.client.get_yield_earned(), 5_000_000);
    // 50M principal + 5M yield is liquid again.
    assert_eq!(balance(&ctx, &ctx.treasury), 55_000_000);
    // The strategy keeps the 50M principal still deployed plus the 5M of yield
    // it has not paid out yet — yield is only credited on recall, never
    // pre-emptively.
    assert_eq!(balance(&ctx, &ctx.aave), 55_000_000);
}

#[test]
fn recalling_everything_empties_a_strategy_position() {
    let ctx = setup();
    configure_two_way(&ctx);
    ctx.client.rebalance_portfolio();
    accrue(&ctx.env, &ctx.token, &ctx.aave, 2_000_000);

    let deployed = ctx.client.get_allocation(&ctx.aave).unwrap().deployed;
    let (principal, yield_amount) = ctx.client.recall_strategy(&ctx.aave, &deployed);
    assert_eq!(principal, deployed);
    assert_eq!(yield_amount, 2_000_000, "the whole accrued yield rides out");

    let alloc = ctx.client.get_allocation(&ctx.aave).unwrap();
    assert_eq!(alloc.deployed, 0);
    assert_eq!(ctx.client.get_deployed_total(), SUPPLY / 2);
    assert_eq!(balance(&ctx, &ctx.treasury), SUPPLY / 2 + 2_000_000);
}

#[test]
fn a_rebalance_books_the_yield_it_recalls() {
    let ctx = setup();
    configure_two_way(&ctx);
    ctx.client.rebalance_portfolio();
    accrue(&ctx.env, &ctx.token, &ctx.aave, 1_000_000);
    accrue(&ctx.env, &ctx.token, &ctx.compound, 3_000_000);

    let deployed = ctx.client.rebalance_portfolio();
    // Yield is recalled, counted into the balance, then re-split — the
    // portfolio is worth strictly more than it was before.
    assert_eq!(deployed, SUPPLY + 4_000_000);
    assert_eq!(ctx.client.get_yield_earned(), 4_000_000);
    assert_eq!(
        ctx.client.get_allocation(&ctx.aave).unwrap().yield_earned,
        1_000_000
    );
    assert_eq!(
        ctx.client
            .get_allocation(&ctx.compound)
            .unwrap()
            .yield_earned,
        3_000_000
    );
    assert_eq!(balance(&ctx, &ctx.treasury), 0);
}

#[test]
fn recalls_beyond_the_deployed_principal_are_rejected() {
    let ctx = setup();
    configure_two_way(&ctx);
    ctx.client.rebalance_portfolio();

    let deployed = ctx.client.get_allocation(&ctx.aave).unwrap().deployed;
    assert_eq!(
        ctx.client.try_recall_strategy(&ctx.aave, &(deployed + 1)),
        Err(Ok(Error::RecallExceedsDeployed))
    );
    assert_eq!(
        ctx.client.try_recall_strategy(&ctx.aave, &0),
        Err(Ok(Error::NothingToRecall))
    );
    // A whitelisted strategy with no allocation at all has no position to
    // recall from.
    let idle = Address::generate(&ctx.env);
    ctx.client.whitelist_strategy(&idle);
    assert_eq!(
        ctx.client.try_recall_strategy(&idle, &1),
        Err(Ok(Error::StrategyNotAllocated))
    );
}

#[test]
fn recalling_from_an_unwhitelisted_strategy_is_rejected() {
    let ctx = setup();
    let outsider = Address::generate(&ctx.env);
    assert_eq!(
        ctx.client.try_recall_strategy(&outsider, &1_000),
        Err(Ok(Error::StrategyNotWhitelisted))
    );
}

// ── Revocation ────────────────────────────────────────────────────────────

#[test]
fn a_strategy_holding_funds_cannot_be_revoked() {
    let ctx = setup();
    configure_two_way(&ctx);
    ctx.client.rebalance_portfolio();

    assert_eq!(
        ctx.client.try_revoke_strategy(&ctx.aave),
        Err(Ok(Error::StrategyHasFunds))
    );

    // Recall everything, and the strategy can be dropped.
    let deployed = ctx.client.get_allocation(&ctx.aave).unwrap().deployed;
    ctx.client.recall_strategy(&ctx.aave, &deployed);
    ctx.client.revoke_strategy(&ctx.aave);
    assert!(!ctx.client.is_strategy_whitelisted(&ctx.aave));
    assert_eq!(ctx.client.get_allocation(&ctx.aave), None);
    assert_eq!(ctx.client.get_allocations().len(), 1);
}

#[test]
fn a_strategy_holding_yield_cannot_be_dropped_from_the_weight_set() {
    let ctx = setup();
    configure_two_way(&ctx);
    ctx.client.rebalance_portfolio();
    accrue(&ctx.env, &ctx.token, &ctx.aave, 1_000);
    let deployed = ctx.client.get_allocation(&ctx.aave).unwrap().deployed;
    ctx.client.recall_strategy(&ctx.aave, &deployed);

    // Principal is back, but the strategy is still owed the accrued yield.
    assert_eq!(
        ctx.client.try_set_allocations(&vec![
            &ctx.env,
            AllocationConfig {
                strategy: ctx.compound.clone(),
                weight_bps: 10_000,
            }
        ]),
        Err(Ok(Error::StrategyHasFunds))
    );
    assert_eq!(
        ctx.client.get_allocations().len(),
        2,
        "state left untouched"
    );
}

#[test]
fn revoking_an_unknown_strategy_is_rejected() {
    let ctx = setup();
    let outsider = Address::generate(&ctx.env);
    assert_eq!(
        ctx.client.try_revoke_strategy(&outsider),
        Err(Ok(Error::StrategyNotWhitelisted))
    );
}

// ── Interaction with vesting claims ───────────────────────────────────────

#[test]
fn a_vesting_claim_recalls_the_portfolio_when_needed() {
    use crate::vesting::ONE_YEAR_SECS;

    let ctx = setup();
    let beneficiary = Address::generate(&ctx.env);
    // A live claim on half the float…
    ctx.client
        .add_team_schedule(&beneficiary, &50_000_000, &SUPPLY_TS);

    // …and governance parks the whole balance in yield strategies anyway.
    configure_two_way(&ctx);
    ctx.client.rebalance_portfolio();
    assert_eq!(balance(&ctx, &ctx.treasury), 0, "float is fully deployed");

    // Two years into the four-year window, half the allocation unlocks. The
    // claim is still payable: the treasury pulls the principal it needs back
    // out of the portfolio.
    set_time(&ctx, SUPPLY_TS + 2 * ONE_YEAR_SECS);
    assert_eq!(ctx.client.vested_amount(&beneficiary), 25_000_000);
    let claimed = ctx.client.claim_vested(&beneficiary);
    assert_eq!(claimed, 25_000_000);
    assert_eq!(balance(&ctx, &beneficiary), 25_000_000);
    assert_eq!(
        balance(&ctx, &ctx.treasury),
        0,
        "the recall covered the payout exactly"
    );
    assert_eq!(ctx.client.get_deployed_total(), SUPPLY - 25_000_000);
    assert_eq!(
        balance(&ctx, &ctx.aave) + balance(&ctx, &ctx.compound),
        SUPPLY - 25_000_000
    );
}

// ── Untrusted strategies ──────────────────────────────────────────────────

#[test]
fn a_strategy_that_under_pays_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let treasury = env.register(Treasury, (admin.clone(), token.clone()));
    let client = TreasuryClient::new(&env, &treasury);
    StellarAssetClient::new(&env, &token).mint(&treasury, &SUPPLY);

    let rogue = env.register(ShortchangingStrategy, (token.clone(), treasury.clone()));
    client.whitelist_strategy(&rogue);
    client.set_allocations(&vec![
        &env,
        AllocationConfig {
            strategy: rogue.clone(),
            weight_bps: 10_000,
        },
    ]);
    client.set_reserve_bps(&0);
    client.rebalance_portfolio();

    // It claims a full return but only sends half: the treasury must not
    // credit principal it never received.
    assert_eq!(
        client.try_recall_strategy(&rogue, &SUPPLY),
        Err(Ok(Error::StrategyUnderpaid))
    );
    assert_eq!(client.get_allocation(&rogue).unwrap().deployed, SUPPLY);
}

#[test]
fn the_strategy_lock_blocks_a_nested_entry_and_then_releases() {
    let ctx = setup();
    let env = &ctx.env;
    let treasury = ctx.treasury.clone();

    // The host already refuses a contract from re-entering its own instance,
    // so the lock is exercised directly: it must reject a nested strategy call
    // that arrives while a rotation is in flight (which a multi-hop
    // strategy → helper → treasury path could still produce).
    let nested = env.as_contract(&treasury, || {
        crate::strategies::with_lock(env, || crate::strategies::with_lock(env, || Ok(())))
    });
    assert_eq!(nested, Err(Error::ReentrancyBlocked));

    // Released once the outer call returns.
    let after = env.as_contract(&treasury, || crate::strategies::with_lock(env, || Ok(1)));
    assert_eq!(after, Ok(1));

    // Also released when the guarded operation itself fails, so one bad
    // strategy cannot wedge the portfolio permanently.
    let failed: Result<(), Error> = env.as_contract(&treasury, || {
        crate::strategies::with_lock(env, || Err(Error::NothingToRecall))
    });
    assert_eq!(failed, Err(Error::NothingToRecall));
    let after_failure = env.as_contract(&treasury, || crate::strategies::with_lock(env, || Ok(2)));
    assert_eq!(after_failure, Ok(2));
}
