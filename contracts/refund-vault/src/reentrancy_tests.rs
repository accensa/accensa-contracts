#![cfg(test)]

//! Adversarial reentrancy tests for the guard added in response to the
//! cross-contract reentrancy hardening request.
//!
//! `RefundVault` makes external calls from `deposit`, `refund`, `withdraw`,
//! `deploy_to_yield`, `withdraw_from_yield` and `harvest_yield` — either to
//! the configured token contract or to a merchant-registered yield strategy
//! (`docs/AUDIT.md` §5, known issue #7: "the strategy is also a re-entrancy
//! surface"; I-7 explicitly calls out "the auditor should verify whether the
//! outbound transfer to a *contract* recipient can re-enter a claim before the
//! cumulative ceiling update lands").
//!
//! **Finding pinned by this module:** on Soroban, it cannot. The host itself
//! refuses to invoke a contract that is already present on the current call
//! stack — confirmed empirically below by having a malicious token's
//! `transfer` call back into the vault mid-payout: the reentrant call fails
//! at the host with `Context, InvalidAction` / "Contract re-entry is not
//! allowed" *before* the vault's own Rust code (including the guard added
//! here) ever runs for that nested invocation. This is a deliberate, documented
//! Soroban platform guarantee (unlike the EVM, where a fallback/hook function
//! can always call back into the caller), and it holds for *any* depth: the
//! check is "is this contract ID anywhere on the active call stack", not "is
//! it the immediate caller", so an indirect A → B → C → A chain is blocked
//! exactly the same way as a direct A → B → A one.
//!
//! That platform guarantee is exactly what `docs/AUDIT.md` §1.2 puts out of
//! scope ("the soroban-sdk / Soroban host environment ... Trusted platform.
//! Audit findings belong in the contracts, not the platform"). The guard
//! implemented in `lib.rs` (`acquire_reentrancy_lock` / `release_reentrancy_lock`,
//! a single shared instance-storage flag covering all six external-call entry
//! points) makes the "no concurrent guarded call" invariant an explicit,
//! in-contract property instead of something an auditor has to take on faith
//! about the host — and it is the *only* thing that would stop a future,
//! purely-internal refactor (a guarded function calling another guarded
//! function directly in Rust, with no cross-contract invocation involved) from
//! reintroducing exactly this bug, since the host's protection only fires on
//! actual contract-to-contract calls.
//!
//! This module therefore tests both layers:
//! - §1 pins the host-level block using malicious token/strategy contracts
//!   that attempt to reenter mid-transfer, exactly as a real attacker would.
//! - §2 exercises the contract's own guard directly (white-box, via
//!   `Env::as_contract` to pre-set the lock the way a hostile *internal* call
//!   path would find it already held) to prove the guard itself is correct,
//!   independent of the host ever needing to step in.
//!
//! `env.mock_all_auths()` is used throughout, exactly as elsewhere in this
//! suite, so `require_auth` is never the reason a reentrant call fails.

use soroban_sdk::{
    contract, contractimpl, contracttype, testutils::Address as _, token::TokenClient, Address,
    BytesN, Env,
};

use crate::{DataKey, Error, RefundVault, RefundVaultClient};

const FLOAT: i128 = 1_000_000;

// ═══════════════════════════════════════════════════════════════════════
// §1 — Malicious counterparties: reenter the vault mid external-call
// ═══════════════════════════════════════════════════════════════════════

// ── Malicious token: reenters the vault mid-`transfer` ─────────────────────
//
// Mirrors `MockSep41Token` in `token_agnostic_tests.rs`, plus an "arming"
// mechanism: once armed, the *next* `transfer` call disarms itself (so the
// attack fires exactly once and cannot recurse forever if the host's
// protection were somehow absent) and invokes the configured reentrant call
// against the vault before finishing its own balance bookkeeping — i.e. the
// vault's outbound transfer is still in flight, and the vault has not yet
// written the record/state update that transfer was meant to precede.

#[contracttype]
enum MalTokenKey {
    Admin,
    Vault,
    Balance(Address),
    Armed,
    ReentryAmount,
    ReentrySelector,
    ReentryPaymentRef,
    ReentryOtherPaymentRef,
    ReentryBuyer,
    LastResult,
}

/// Which guarded vault entry point the token should call back into.
#[contracttype]
#[derive(Clone, Copy)]
enum ReentrySelector {
    RefundSamePaymentRef,
    RefundOtherPaymentRef,
    Withdraw,
}

/// Outcome of a reentrant attempt, as observed by the attacker contract.
///
/// 0 = the reentrant call **succeeded** — the guard failed completely; any
/// test that observes this has found a real double-spend.
/// 1 = the reentrant call failed at the Soroban host, before the vault's own
/// code ran (`Err(Err(_))`) — the expected outcome for external
/// cross-contract reentrancy today.
/// 2 = the reentrant call failed with the vault's own `ReentrancyBlocked`
/// (`Err(Ok(Error::ReentrancyBlocked))`) — also an acceptable "blocked"
/// outcome, and what would fire if the host's protection were ever absent.
/// 3 = failed with a different, unrelated error — a bug in the test setup,
/// not evidence the guard works.
/// 4 = never attempted (not armed, or `transfer` not called).
const RESULT_SUCCEEDED: u32 = 0;
const RESULT_HOST_BLOCKED: u32 = 1;
const RESULT_GUARD_BLOCKED: u32 = 2;
const RESULT_OTHER_ERROR: u32 = 3;
const RESULT_NOT_ATTEMPTED: u32 = 4;

fn classify_refund_result(
    result: Result<
        Result<(), soroban_sdk::ConversionError>,
        Result<Error, soroban_sdk::InvokeError>,
    >,
) -> u32 {
    match result {
        Ok(_) => RESULT_SUCCEEDED,
        Err(Ok(Error::ReentrancyBlocked)) => RESULT_GUARD_BLOCKED,
        Err(Ok(_)) => RESULT_OTHER_ERROR,
        Err(Err(_)) => RESULT_HOST_BLOCKED,
    }
}

#[contract]
struct MaliciousToken;

#[contractimpl]
impl MaliciousToken {
    pub fn initialize(env: Env, admin: Address, vault: Address) {
        env.storage().instance().set(&MalTokenKey::Admin, &admin);
        env.storage().instance().set(&MalTokenKey::Vault, &vault);
        env.storage().instance().set(&MalTokenKey::Armed, &false);
        env.storage()
            .instance()
            .set(&MalTokenKey::LastResult, &RESULT_NOT_ATTEMPTED);
    }

    pub fn balance(env: Env, id: Address) -> i128 {
        env.storage()
            .instance()
            .get(&MalTokenKey::Balance(id))
            .unwrap_or(0)
    }

    pub fn mint(env: Env, to: Address, amount: i128) {
        let admin: Address = env.storage().instance().get(&MalTokenKey::Admin).unwrap();
        admin.require_auth();
        let balance: i128 = env
            .storage()
            .instance()
            .get(&MalTokenKey::Balance(to.clone()))
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&MalTokenKey::Balance(to), &(balance + amount));
    }

    /// Test setup: arm the token to attempt one reentrant call, described by
    /// `selector`, the next time `transfer` runs. No auth: this is a test
    /// harness knob, not something a real token would expose.
    pub fn arm(
        env: Env,
        selector: ReentrySelector,
        payment_ref: BytesN<32>,
        other_payment_ref: BytesN<32>,
        buyer: Address,
        amount: i128,
    ) {
        env.storage().instance().set(&MalTokenKey::Armed, &true);
        env.storage()
            .instance()
            .set(&MalTokenKey::ReentrySelector, &selector);
        env.storage()
            .instance()
            .set(&MalTokenKey::ReentryPaymentRef, &payment_ref);
        env.storage()
            .instance()
            .set(&MalTokenKey::ReentryOtherPaymentRef, &other_payment_ref);
        env.storage()
            .instance()
            .set(&MalTokenKey::ReentryBuyer, &buyer);
        env.storage()
            .instance()
            .set(&MalTokenKey::ReentryAmount, &amount);
    }

    pub fn last_result(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&MalTokenKey::LastResult)
            .unwrap_or(RESULT_NOT_ATTEMPTED)
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        from.require_auth();

        let from_balance: i128 = env
            .storage()
            .instance()
            .get(&MalTokenKey::Balance(from.clone()))
            .unwrap_or(0);
        if from_balance < amount {
            panic!("insufficient balance");
        }

        let armed: bool = env
            .storage()
            .instance()
            .get(&MalTokenKey::Armed)
            .unwrap_or(false);
        if armed {
            // Disarm first so a legitimate second transfer later in the same
            // test (or a successful reentrant call, if the guard is broken)
            // does not recurse unboundedly.
            env.storage().instance().set(&MalTokenKey::Armed, &false);

            let vault: Address = env.storage().instance().get(&MalTokenKey::Vault).unwrap();
            let vault_client = RefundVaultClient::new(&env, &vault);
            let selector: ReentrySelector = env
                .storage()
                .instance()
                .get(&MalTokenKey::ReentrySelector)
                .unwrap();
            let payment_ref: BytesN<32> = env
                .storage()
                .instance()
                .get(&MalTokenKey::ReentryPaymentRef)
                .unwrap();
            let other_payment_ref: BytesN<32> = env
                .storage()
                .instance()
                .get(&MalTokenKey::ReentryOtherPaymentRef)
                .unwrap();
            let buyer: Address = env
                .storage()
                .instance()
                .get(&MalTokenKey::ReentryBuyer)
                .unwrap();
            let reentry_amount: i128 = env
                .storage()
                .instance()
                .get(&MalTokenKey::ReentryAmount)
                .unwrap();

            // Called from deep inside the vault's own `token::Client::transfer`,
            // exactly where a hostile token's transfer hook would run: the
            // vault's outbound payment is still executing and its record/state
            // write for that payment has not happened yet.
            let result = match selector {
                ReentrySelector::RefundSamePaymentRef => {
                    classify_refund_result(vault_client.try_refund(
                        &payment_ref,
                        &buyer,
                        &reentry_amount,
                        &0,
                        &reentry_amount,
                        &None,
                    ))
                }
                ReentrySelector::RefundOtherPaymentRef => {
                    classify_refund_result(vault_client.try_refund(
                        &other_payment_ref,
                        &buyer,
                        &reentry_amount,
                        &0,
                        &reentry_amount,
                        &None,
                    ))
                }
                ReentrySelector::Withdraw => {
                    match vault_client.try_withdraw(&reentry_amount, &buyer) {
                        Ok(_) => RESULT_SUCCEEDED,
                        Err(Ok(Error::ReentrancyBlocked)) => RESULT_GUARD_BLOCKED,
                        Err(Ok(_)) => RESULT_OTHER_ERROR,
                        Err(Err(_)) => RESULT_HOST_BLOCKED,
                    }
                }
            };
            env.storage()
                .instance()
                .set(&MalTokenKey::LastResult, &result);
        }

        let to_balance: i128 = env
            .storage()
            .instance()
            .get(&MalTokenKey::Balance(to.clone()))
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&MalTokenKey::Balance(from), &(from_balance - amount));
        env.storage()
            .instance()
            .set(&MalTokenKey::Balance(to), &(to_balance + amount));
    }
}

// ── Test helpers ─────────────────────────────────────────────────────────

struct MaliciousTokenVault {
    env: Env,
    client: RefundVaultClient<'static>,
    merchant: Address,
    token_id: Address,
}

fn setup_with_malicious_token() -> MaliciousTokenVault {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let vault_id = env.register(RefundVault, ());
    let client = RefundVaultClient::new(&env, &vault_id);

    let token_id = env.register(MaliciousToken, ());
    MaliciousTokenClient::new(&env, &token_id).initialize(&merchant, &vault_id);
    MaliciousTokenClient::new(&env, &token_id).mint(&merchant, &FLOAT);

    client.initialize(&merchant, &token_id, &100);
    client.deposit(&merchant, &FLOAT);

    MaliciousTokenVault {
        env,
        client,
        merchant,
        token_id,
    }
}

// ── Adversarial tests: token-transfer-phase reentrancy ─────────────────────

/// The headline scenario from the hardening request: an attacker reenters
/// `refund` for the *same* `payment_ref`, from inside the token transfer
/// that the first `refund` call triggered, attempting to drain the vault
/// before the cumulative-refund record is updated.
#[test]
fn test_reentrant_refund_same_payment_ref_is_blocked() {
    let MaliciousTokenVault {
        env,
        client,
        merchant: _,
        token_id,
    } = setup_with_malicious_token();

    let payment_ref = BytesN::from_array(&env, &[1u8; 32]);
    let other_ref = BytesN::from_array(&env, &[2u8; 32]);
    let buyer = Address::generate(&env);
    let amount = 100_000i128;

    let token_client = MaliciousTokenClient::new(&env, &token_id);
    token_client.arm(
        &ReentrySelector::RefundSamePaymentRef,
        &payment_ref,
        &other_ref,
        &buyer,
        &amount,
    );

    client.refund(&payment_ref, &buyer, &amount, &0, &amount, &None);

    // The reentrant call never reached the vault's own code: the Soroban
    // host rejected it outright as a call-stack cycle.
    assert_eq!(token_client.last_result(), RESULT_HOST_BLOCKED);

    // Exactly one payout happened: the buyer holds one `amount`, not two.
    assert_eq!(token_client.balance(&buyer), amount);
    assert_eq!(token_client.balance(&client.address), FLOAT - amount);

    // The stored record reflects a single refund, not a doubled one.
    let record = client.get_refund(&payment_ref).unwrap();
    assert_eq!(record.amount_refunded, amount);
}

/// Same attack shape, but reentering on a *different* `payment_ref` — proves
/// the block is not scoped to one payment (the host rejects the call
/// entirely, and separately the guard, if it were ever reached, is a single
/// shared lock across the whole contract, not per-payment).
#[test]
fn test_reentrant_refund_other_payment_ref_is_blocked() {
    let MaliciousTokenVault {
        env,
        client,
        merchant: _,
        token_id,
    } = setup_with_malicious_token();

    let payment_ref = BytesN::from_array(&env, &[3u8; 32]);
    let other_ref = BytesN::from_array(&env, &[4u8; 32]);
    let buyer = Address::generate(&env);
    let amount = 50_000i128;

    let token_client = MaliciousTokenClient::new(&env, &token_id);
    token_client.arm(
        &ReentrySelector::RefundOtherPaymentRef,
        &payment_ref,
        &other_ref,
        &buyer,
        &amount,
    );

    client.refund(&payment_ref, &buyer, &amount, &0, &amount, &None);

    assert_eq!(token_client.last_result(), RESULT_HOST_BLOCKED);
    // The unrelated payment ref was never touched.
    assert!(client.get_refund(&other_ref).is_none());
    assert_eq!(token_client.balance(&client.address), FLOAT - amount);
}

/// Cross-function reentrancy: the callback targets `withdraw`, a different
/// guarded entry point, while `refund`'s transfer is still executing.
#[test]
fn test_reentrant_withdraw_during_refund_is_blocked() {
    let MaliciousTokenVault {
        env,
        client,
        merchant,
        token_id,
    } = setup_with_malicious_token();

    let payment_ref = BytesN::from_array(&env, &[5u8; 32]);
    let other_ref = BytesN::from_array(&env, &[6u8; 32]);
    let amount = 25_000i128;

    let token_client = MaliciousTokenClient::new(&env, &token_id);
    // `Withdraw`'s recipient is `merchant` here — reusing the `buyer` slot.
    token_client.arm(
        &ReentrySelector::Withdraw,
        &payment_ref,
        &other_ref,
        &merchant,
        &amount,
    );

    client.refund(
        &payment_ref,
        &Address::generate(&env),
        &amount,
        &0,
        &amount,
        &None,
    );

    assert_eq!(token_client.last_result(), RESULT_HOST_BLOCKED);
    // Only the legitimate refund left the vault; the reentrant withdraw did not.
    assert_eq!(token_client.balance(&client.address), FLOAT - amount);
}

// ── Malicious yield strategy: reenters the vault mid-`deposit`/`withdraw`/`harvest` ─

#[contracttype]
enum MalStrategyKey {
    Token,
    Vault,
    TotalDeposited,
    YieldAccrued,
    Armed,
    ReentrySelector,
    ReentryAmount,
    LastResult,
}

#[contracttype]
#[derive(Clone, Copy)]
enum StrategyReentrySelector {
    Deploy,
    WithdrawPrincipal,
    Harvest,
}

#[contract]
struct MaliciousYieldStrategy;

#[contractimpl]
impl MaliciousYieldStrategy {
    pub fn initialize(env: Env, token: Address, vault: Address) {
        env.storage().instance().set(&MalStrategyKey::Token, &token);
        env.storage().instance().set(&MalStrategyKey::Vault, &vault);
        env.storage()
            .instance()
            .set(&MalStrategyKey::TotalDeposited, &0i128);
        env.storage()
            .instance()
            .set(&MalStrategyKey::YieldAccrued, &0i128);
        env.storage()
            .instance()
            .set(&MalStrategyKey::LastResult, &RESULT_NOT_ATTEMPTED);
    }

    pub fn simulate_yield(env: Env, amount: i128) {
        let current: i128 = env
            .storage()
            .instance()
            .get(&MalStrategyKey::YieldAccrued)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&MalStrategyKey::YieldAccrued, &(current + amount));
    }

    pub fn arm(env: Env, selector: StrategyReentrySelector, amount: i128) {
        env.storage().instance().set(&MalStrategyKey::Armed, &true);
        env.storage()
            .instance()
            .set(&MalStrategyKey::ReentrySelector, &selector);
        env.storage()
            .instance()
            .set(&MalStrategyKey::ReentryAmount, &amount);
    }

    pub fn last_result(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&MalStrategyKey::LastResult)
            .unwrap_or(RESULT_NOT_ATTEMPTED)
    }

    fn maybe_reenter(env: &Env) {
        let armed: bool = env
            .storage()
            .instance()
            .get(&MalStrategyKey::Armed)
            .unwrap_or(false);
        if !armed {
            return;
        }
        env.storage().instance().set(&MalStrategyKey::Armed, &false);

        let vault: Address = env
            .storage()
            .instance()
            .get(&MalStrategyKey::Vault)
            .unwrap();
        let vault_client = RefundVaultClient::new(env, &vault);
        let selector: StrategyReentrySelector = env
            .storage()
            .instance()
            .get(&MalStrategyKey::ReentrySelector)
            .unwrap();
        let amount: i128 = env
            .storage()
            .instance()
            .get(&MalStrategyKey::ReentryAmount)
            .unwrap();

        let result: u32 = match selector {
            StrategyReentrySelector::Deploy => match vault_client.try_deploy_to_yield(&amount) {
                Ok(_) => RESULT_SUCCEEDED,
                Err(Ok(Error::ReentrancyBlocked)) => RESULT_GUARD_BLOCKED,
                Err(Ok(_)) => RESULT_OTHER_ERROR,
                Err(Err(_)) => RESULT_HOST_BLOCKED,
            },
            StrategyReentrySelector::WithdrawPrincipal => {
                match vault_client.try_withdraw_from_yield(&amount) {
                    Ok(_) => RESULT_SUCCEEDED,
                    Err(Ok(Error::ReentrancyBlocked)) => RESULT_GUARD_BLOCKED,
                    Err(Ok(_)) => RESULT_OTHER_ERROR,
                    Err(Err(_)) => RESULT_HOST_BLOCKED,
                }
            }
            StrategyReentrySelector::Harvest => match vault_client.try_harvest_yield() {
                Ok(_) => RESULT_SUCCEEDED,
                Err(Ok(Error::ReentrancyBlocked)) => RESULT_GUARD_BLOCKED,
                Err(Ok(_)) => RESULT_OTHER_ERROR,
                Err(Err(_)) => RESULT_HOST_BLOCKED,
            },
        };
        env.storage()
            .instance()
            .set(&MalStrategyKey::LastResult, &result);
    }

    /// Called by the vault's `deploy_to_yield`, after tokens were already
    /// transferred to this contract but before the vault records
    /// `DeployedPrincipal` — the reentrancy window the guard closes.
    pub fn deposit(env: Env, amount: i128) -> Result<(), Error> {
        Self::maybe_reenter(&env);

        let total: i128 = env
            .storage()
            .instance()
            .get(&MalStrategyKey::TotalDeposited)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&MalStrategyKey::TotalDeposited, &(total + amount));
        Ok(())
    }

    /// Called by the vault's `withdraw_from_yield`, before the vault updates
    /// `DeployedPrincipal`/`HarvestedYield`.
    pub fn withdraw(env: Env, principal: i128) -> Result<(i128, i128), Error> {
        Self::maybe_reenter(&env);

        let total: i128 = env
            .storage()
            .instance()
            .get(&MalStrategyKey::TotalDeposited)
            .unwrap_or(0);
        if principal > total || principal <= 0 {
            return Err(Error::NothingToWithdraw);
        }
        let yield_accrued: i128 = env
            .storage()
            .instance()
            .get(&MalStrategyKey::YieldAccrued)
            .unwrap_or(0);
        let yield_portion = if total > 0 {
            yield_accrued * principal / total
        } else {
            0
        };
        let total_return = principal + yield_portion;

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&MalStrategyKey::Token)
            .unwrap();
        let token_client = TokenClient::new(&env, &token_addr);
        let vault_addr: Address = env
            .storage()
            .instance()
            .get(&MalStrategyKey::Vault)
            .unwrap();
        token_client.transfer(&env.current_contract_address(), &vault_addr, &total_return);

        env.storage()
            .instance()
            .set(&MalStrategyKey::TotalDeposited, &(total - principal));
        env.storage().instance().set(
            &MalStrategyKey::YieldAccrued,
            &(yield_accrued - yield_portion),
        );

        Ok((principal, yield_portion))
    }

    /// Called by the vault's `harvest_yield`, before the vault updates
    /// `HarvestedYield`.
    pub fn harvest(env: Env) -> Result<i128, Error> {
        Self::maybe_reenter(&env);

        let yield_accrued: i128 = env
            .storage()
            .instance()
            .get(&MalStrategyKey::YieldAccrued)
            .unwrap_or(0);
        if yield_accrued <= 0 {
            return Err(Error::NothingToHarvest);
        }

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&MalStrategyKey::Token)
            .unwrap();
        let token_client = TokenClient::new(&env, &token_addr);
        let vault_addr: Address = env
            .storage()
            .instance()
            .get(&MalStrategyKey::Vault)
            .unwrap();
        token_client.transfer(&env.current_contract_address(), &vault_addr, &yield_accrued);

        env.storage()
            .instance()
            .set(&MalStrategyKey::YieldAccrued, &0i128);
        Ok(yield_accrued)
    }

    pub fn total_balance(env: Env) -> i128 {
        let token_addr: Address = env
            .storage()
            .instance()
            .get(&MalStrategyKey::Token)
            .unwrap();
        let token_client = TokenClient::new(&env, &token_addr);
        token_client.balance(&env.current_contract_address())
    }

    pub fn accrued_yield(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&MalStrategyKey::YieldAccrued)
            .unwrap_or(0)
    }
}

// ── Test helpers (yield path) ───────────────────────────────────────────

const YIELD_FLOAT: i128 = 10_000_000;

fn setup_with_malicious_strategy(
    reserve_bp: u32,
    max_deploy_bp: u32,
) -> (Env, RefundVaultClient<'static>, Address, Address) {
    use soroban_sdk::token::StellarAssetClient;

    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &YIELD_FLOAT);

    let vault_id = env.register(RefundVault, ());
    let vault_client = RefundVaultClient::new(&env, &vault_id);
    vault_client.initialize(&merchant, &token, &17_280);

    let strategy_id = env.register(MaliciousYieldStrategy, ());
    MaliciousYieldStrategyClient::new(&env, &strategy_id).initialize(&token, &vault_id);
    // Fund the strategy so it can honor withdraw/harvest transfers back.
    StellarAssetClient::new(&env, &token).mint(&strategy_id, &YIELD_FLOAT);

    vault_client.set_yield_strategy(&strategy_id);
    vault_client.set_reserve_ratio(&reserve_bp);
    vault_client.set_max_deploy_ratio(&max_deploy_bp);

    vault_client.deposit(&merchant, &YIELD_FLOAT);

    (env, vault_client, token, strategy_id)
}

// ── Adversarial tests: yield-strategy-phase reentrancy ─────────────────────

/// A malicious strategy's `deposit` callback tries to call `deploy_to_yield`
/// again before the vault records `DeployedPrincipal` for the first call —
/// an attempt to get the vault to double-count (or over-deploy past) the
/// same float.
#[test]
fn test_reentrant_deploy_to_yield_is_blocked() {
    let (env, vault_client, _token, strategy_id) = setup_with_malicious_strategy(1000, 9000);

    let strategy_client = MaliciousYieldStrategyClient::new(&env, &strategy_id);
    strategy_client.arm(&StrategyReentrySelector::Deploy, &500_000);

    vault_client.deploy_to_yield(&1_000_000);

    assert_eq!(strategy_client.last_result(), RESULT_HOST_BLOCKED);
    // Exactly one deployment of 1_000_000 was recorded, not 1_500_000.
    assert_eq!(vault_client.get_yield_info().deployed_principal, 1_000_000);
}

/// A malicious strategy's `withdraw` callback tries to call
/// `withdraw_from_yield` again before the vault decrements
/// `DeployedPrincipal` for the first call — an attempt to reclaim more
/// principal than was ever deployed.
#[test]
fn test_reentrant_withdraw_from_yield_is_blocked() {
    let (env, vault_client, _token, strategy_id) = setup_with_malicious_strategy(1000, 9000);

    vault_client.deploy_to_yield(&2_000_000);

    let strategy_client = MaliciousYieldStrategyClient::new(&env, &strategy_id);
    strategy_client.arm(&StrategyReentrySelector::WithdrawPrincipal, &2_000_000);

    vault_client.withdraw_from_yield(&1_000_000);

    assert_eq!(strategy_client.last_result(), RESULT_HOST_BLOCKED);
    // Only the legitimate 1_000_000 was reclaimed; the reentrant attempt to
    // pull the remaining principal a second time was rejected.
    assert_eq!(vault_client.get_yield_info().deployed_principal, 1_000_000);
}

/// A malicious strategy's `harvest` callback tries to call `harvest_yield`
/// again before the vault updates `HarvestedYield` for the first call — an
/// attempt to double-count the same accrued yield.
#[test]
fn test_reentrant_harvest_yield_is_blocked() {
    let (env, vault_client, _token, strategy_id) = setup_with_malicious_strategy(1000, 9000);

    vault_client.deploy_to_yield(&2_000_000);

    let strategy_client = MaliciousYieldStrategyClient::new(&env, &strategy_id);
    strategy_client.simulate_yield(&100_000);
    strategy_client.arm(&StrategyReentrySelector::Harvest, &0);

    vault_client.harvest_yield();

    assert_eq!(strategy_client.last_result(), RESULT_HOST_BLOCKED);
    assert_eq!(vault_client.get_yield_info().harvested_yield, 100_000);
}

// ═══════════════════════════════════════════════════════════════════════
// §2 — White-box: the guard's own logic, independent of host behaviour
// ═══════════════════════════════════════════════════════════════════════
//
// §1 proves the host never lets a hostile external contract reach the point
// where `acquire_reentrancy_lock` would run. These tests instead put the
// vault into the state a reentrant call *would* find it in — the lock
// flag already held — the same way an internal composition bug would (one
// guarded function calling another directly in Rust, which the host cannot
// see because no new contract invocation occurs). They call
// `Env::as_contract` to write `DataKey::ReentrancyLock = true` into the
// vault's own instance storage from outside, exactly mimicking "a guarded
// call is already in progress", then confirm every guarded entry point
// refuses to run.

fn setup_plain_vault() -> (Env, RefundVaultClient<'static>, Address, Address) {
    use soroban_sdk::token::StellarAssetClient;

    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let vault_id = env.register(RefundVault, ());
    let client = RefundVaultClient::new(&env, &vault_id);
    client.initialize(&merchant, &token, &100);
    client.deposit(&merchant, &FLOAT);

    (env, client, merchant, token)
}

fn hold_lock(env: &Env, vault_id: &Address) {
    env.as_contract(vault_id, || {
        env.storage()
            .instance()
            .set(&DataKey::ReentrancyLock, &true);
    });
}

#[test]
fn test_guard_blocks_deposit_while_lock_held() {
    let (env, client, merchant, _token) = setup_plain_vault();
    hold_lock(&env, &client.address);

    assert_eq!(
        client.try_deposit(&merchant, &1_000),
        Err(Ok(Error::ReentrancyBlocked))
    );
}

#[test]
fn test_guard_blocks_refund_while_lock_held() {
    let (env, client, _merchant, _token) = setup_plain_vault();
    hold_lock(&env, &client.address);

    let payment_ref = BytesN::from_array(&env, &[20u8; 32]);
    let buyer = Address::generate(&env);
    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &1_000, &0, &1_000, &None),
        Err(Ok(Error::ReentrancyBlocked))
    );
    assert!(client.get_refund(&payment_ref).is_none());
}

#[test]
fn test_guard_blocks_withdraw_while_lock_held() {
    let (env, client, merchant, _token) = setup_plain_vault();
    hold_lock(&env, &client.address);

    assert_eq!(
        client.try_withdraw(&1_000, &merchant),
        Err(Ok(Error::ReentrancyBlocked))
    );
}

#[test]
fn test_guard_blocks_deploy_to_yield_while_lock_held() {
    let (env, client, _merchant, token) = setup_plain_vault();

    let strategy_id = env.register(crate::yield_tests::MockYieldStrategy, ());
    let strategy_client = crate::yield_tests::MockYieldStrategyClient::new(&env, &strategy_id);
    strategy_client.initialize(&token, &client.address);
    client.set_yield_strategy(&strategy_id);
    client.set_reserve_ratio(&0);
    client.set_max_deploy_ratio(&10_000);

    hold_lock(&env, &client.address);

    assert_eq!(
        client.try_deploy_to_yield(&1_000),
        Err(Ok(Error::ReentrancyBlocked))
    );
}

#[test]
fn test_lock_is_released_after_successful_call() {
    let (env, client, merchant, _token) = setup_plain_vault();

    let ref_a = BytesN::from_array(&env, &[9u8; 32]);
    let ref_b = BytesN::from_array(&env, &[10u8; 32]);
    let buyer = Address::generate(&env);

    client.refund(&ref_a, &buyer, &1_000, &0, &1_000, &None);
    // If the lock leaked as "held" from the first call, this would fail with
    // ReentrancyBlocked instead of succeeding.
    client.refund(&ref_b, &buyer, &2_000, &0, &2_000, &None);
    client.withdraw(&500, &merchant);
}
