#![cfg(test)]

//! Token-agnosticism tests (issue #166).
//!
//! `RefundVault` is generic over SEP-41 tokens by construction — `initialize`
//! takes any token contract address — but that generality is only exercised by
//! the default 7-decimal Stellar Asset Contract in the rest of the suite. The
//! Stellar Asset Contract is always 7-decimal, so it can never prove the vault
//! against a token with different precision. This module registers a minimal
//! SEP-41 token with configurable decimals (see `MockSep41Token`) and runs the
//! full lifecycle — deposit, refund, withdraw and the float-bound check —
//! against it, plus the boundary cases: the smallest unit, i128 extremes, and a
//! refund exactly equal to the float.
//!
//! The conclusion these tests pin is deliberate and documented in
//! `docs/storage-audit.md` (Token Generality): the vault never assumes seven
//! decimals. All amounts are raw integer units in the token's smallest unit, and
//! the float-bound check compares those units directly against the vault's token
//! balance, so a 0-, 2- or 7-decimal token behaves identically. Converting
//! human-readable amounts to the token's smallest unit is the merchant's (and
//! the facilitator's) responsibility, not the contract's.

use soroban_sdk::{
    contract, contractimpl, contracttype, testutils::Address as _, token::TokenClient, Address,
    BytesN, Env,
};

use crate::test_helpers::vault_init;
use crate::{Error, RefundVault, RefundVaultClient};

const FLOAT: i128 = 1_000_000;

// ── Minimal SEP-41 token with configurable decimals ────────────────────────

#[contracttype]
enum MockTokenDataKey {
    Admin,
    Decimals,
    Balance(Address),
}

#[contract]
struct MockSep41Token;

#[contractimpl]
impl MockSep41Token {
    pub fn initialize(env: Env, admin: Address, decimals: u32) {
        env.storage()
            .instance()
            .set(&MockTokenDataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&MockTokenDataKey::Decimals, &decimals);
    }

    /// SEP-41: number of decimals used to represent amounts of this token.
    pub fn decimals(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&MockTokenDataKey::Decimals)
            .unwrap()
    }

    /// SEP-41: balance of `id`, or 0 for unknown addresses.
    pub fn balance(env: Env, id: Address) -> i128 {
        env.storage()
            .instance()
            .get(&MockTokenDataKey::Balance(id))
            .unwrap_or(0)
    }

    /// SEP-41: transfer `amount` from `from` to `to`, authorised by `from`.
    /// Panics if `from` lacks the balance, mirroring the SAC's behaviour.
    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        from.require_auth();

        let from_balance: i128 = env
            .storage()
            .instance()
            .get(&MockTokenDataKey::Balance(from.clone()))
            .unwrap_or(0);
        if from_balance < amount {
            panic!("insufficient balance");
        }
        let to_balance: i128 = env
            .storage()
            .instance()
            .get(&MockTokenDataKey::Balance(to.clone()))
            .unwrap_or(0);

        env.storage()
            .instance()
            .set(&MockTokenDataKey::Balance(from), &(from_balance - amount));
        env.storage()
            .instance()
            .set(&MockTokenDataKey::Balance(to), &(to_balance + amount));
    }

    /// Admin-only test convenience (minting is not part of SEP-41).
    pub fn mint(env: Env, to: Address, amount: i128) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&MockTokenDataKey::Admin)
            .unwrap();
        admin.require_auth();

        let balance: i128 = env
            .storage()
            .instance()
            .get(&MockTokenDataKey::Balance(to.clone()))
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&MockTokenDataKey::Balance(to), &(balance + amount));
    }
}

// ── Test helpers ───────────────────────────────────────────────────────────

struct VaultWithToken {
    env: Env,
    client: RefundVaultClient<'static>,
    merchant: Address,
    token_client: TokenClient<'static>,
}

fn setup_with_token_and_float(decimals: u32, float: i128) -> VaultWithToken {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token_admin = Address::generate(&env);

    let token_id = env.register(MockSep41Token, ());
    let token = token_id.clone();
    MockSep41TokenClient::new(&env, &token_id).initialize(&token_admin, &decimals);
    MockSep41TokenClient::new(&env, &token_id).mint(&merchant, &float);

    let vault_id = env.register(RefundVault, (vault_init(&env, &merchant, &token, 100),));
    let client = RefundVaultClient::new(&env, &vault_id);

    let token_client = TokenClient::new(&env, &token);
    VaultWithToken {
        env,
        client,
        merchant,
        token_client,
    }
}

fn setup_with_token(decimals: u32) -> VaultWithToken {
    setup_with_token_and_float(decimals, FLOAT)
}

// ── Full lifecycle against a non-7-decimal asset ───────────────────────────

#[test]
fn test_full_lifecycle_with_zero_decimal_token() {
    let VaultWithToken {
        env,
        client,
        merchant,
        token_client,
    } = setup_with_token(0);

    // This mock really is a non-7-decimal asset: 1 unit == 1 whole token.
    assert_eq!(token_client.decimals(), 0);

    // Deposit: the merchant funds the float in whole-token units.
    client.deposit(&merchant, &1_000);
    assert_eq!(token_client.balance(&client.address), 1_000);
    assert_eq!(token_client.balance(&merchant), FLOAT - 1_000);

    // Refund the smallest representable unit (1) to a buyer.
    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let buyer = Address::generate(&env);
    client.refund(&payment_ref, &buyer, &1, &0, &1, &None, &0);
    assert_eq!(token_client.balance(&buyer), 1);
    assert_eq!(token_client.balance(&client.address), 999);

    // Withdraw the remaining float back to the merchant.
    client.withdraw(&999, &merchant);
    assert_eq!(token_client.balance(&client.address), 0);
    assert_eq!(token_client.balance(&merchant), FLOAT - 1);
}

#[test]
fn test_full_lifecycle_with_two_decimal_token() {
    let VaultWithToken {
        env,
        client,
        merchant,
        token_client,
    } = setup_with_token(2);

    assert_eq!(token_client.decimals(), 2);

    // 12_345 units == 123.45 whole tokens at 2 decimals.
    client.deposit(&merchant, &12_345);
    assert_eq!(token_client.balance(&client.address), 12_345);

    // Refund 0.45 tokens, then withdraw the remaining 123.00 tokens.
    let payment_ref = BytesN::from_array(&env, &[8u8; 32]);
    let buyer = Address::generate(&env);
    client.refund(&payment_ref, &buyer, &45, &0, &45, &None, &0);
    client.withdraw(&12_300, &merchant);

    assert_eq!(token_client.balance(&buyer), 45);
    assert_eq!(token_client.balance(&client.address), 0);
}

#[test]
fn test_float_bound_check_with_non_7_decimal_token() {
    let VaultWithToken {
        env,
        client,
        merchant,
        token_client: _,
    } = setup_with_token(0);

    client.deposit(&merchant, &100);

    // The float-bound check compares raw units, exactly as it does for the SAC.
    let payment_ref = BytesN::from_array(&env, &[14u8; 32]);
    let buyer = Address::generate(&env);
    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &101, &0, &101, &None, &0),
        Err(Ok(Error::InsufficientFloat))
    );
}

// ── Boundary cases ─────────────────────────────────────────────────────────

#[test]
fn test_refund_exactly_equal_to_float_succeeds() {
    let VaultWithToken {
        env,
        client,
        merchant,
        token_client,
    } = setup_with_token(0);

    client.deposit(&merchant, &1_000);

    // A refund equal to the entire float is allowed...
    let payment_ref = BytesN::from_array(&env, &[9u8; 32]);
    let buyer = Address::generate(&env);
    client.refund(&payment_ref, &buyer, &1_000, &0, &1_000, &None, &0);
    assert_eq!(token_client.balance(&client.address), 0);

    // ...and any further refund is bounded by the now-empty float.
    let payment_ref2 = BytesN::from_array(&env, &[10u8; 32]);
    assert_eq!(
        client.try_refund(&payment_ref2, &buyer, &1, &0, &1, &None, &1),
        Err(Ok(Error::InsufficientFloat))
    );
}

#[test]
fn test_smallest_unit_round_trip() {
    let VaultWithToken {
        env,
        client,
        merchant,
        token_client,
    } = setup_with_token(0);

    // 1 is the smallest representable unit of a 0-decimal token; the vault must
    // handle it in every direction (deposit, refund, withdraw).
    client.deposit(&merchant, &2);
    assert_eq!(token_client.balance(&client.address), 2);

    let payment_ref = BytesN::from_array(&env, &[11u8; 32]);
    let buyer = Address::generate(&env);
    client.refund(&payment_ref, &buyer, &1, &0, &1, &None, &0);
    assert_eq!(token_client.balance(&buyer), 1);

    client.withdraw(&1, &merchant);
    assert_eq!(token_client.balance(&client.address), 0);
}

#[test]
fn test_i128_extreme_deposit_and_withdraw() {
    let extreme = i128::MAX;
    let VaultWithToken {
        env,
        client,
        merchant,
        token_client,
    } = setup_with_token_and_float(0, extreme);

    client.deposit(&merchant, &extreme);
    assert_eq!(token_client.balance(&client.address), extreme);

    // Withdraw the full i128 range to a third party.
    let recipient = Address::generate(&env);
    client.withdraw(&extreme, &recipient);
    assert_eq!(token_client.balance(&recipient), extreme);
    assert_eq!(token_client.balance(&client.address), 0);
}

#[test]
fn test_i128_extreme_refund() {
    let extreme = i128::MAX;
    let VaultWithToken {
        env,
        client,
        merchant,
        token_client,
    } = setup_with_token_and_float(0, extreme);

    client.deposit(&merchant, &extreme);

    // A refund of the entire i128 range — the vault must not overflow or
    // miscompare at the boundary of its integer type.
    let payment_ref = BytesN::from_array(&env, &[12u8; 32]);
    let buyer = Address::generate(&env);
    client.refund(&payment_ref, &buyer, &extreme, &0, &extreme, &None, &0);
    assert_eq!(token_client.balance(&buyer), extreme);
    assert_eq!(token_client.balance(&client.address), 0);

    // One unit over the empty float is rejected by the bound check.
    let payment_ref2 = BytesN::from_array(&env, &[13u8; 32]);
    assert_eq!(
        client.try_refund(&payment_ref2, &buyer, &1, &0, &1, &None, &1),
        Err(Ok(Error::InsufficientFloat))
    );
}
