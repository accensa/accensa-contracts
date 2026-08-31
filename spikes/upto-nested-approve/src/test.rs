#![cfg(test)]
//! # Spike Tests: Nested Authorization for UptoAuthorization
//!
//! These tests empirically investigate whether a single Soroban auth entry
//! can cover a parent contract invocation with a nested SEP-41 token approve.
//!
//! **Approach:**
//! - All setups use `mock_all_auths()` for the token minting that happens in setup.
//! - Tests use `env.auths()` to inspect the recorded authorization tree.
//! - `mock_all_auths()` puts the host in **recording** mode: all `require_auth`
//!   calls succeed and are recorded. `env.auths()` returns the recorded tree.
//! - This is sufficient to prove the auth tree structure Soroban creates and
//!   that a single payer auth entry covers the nested approve.

use super::*;
use soroban_sdk::{
    testutils::{Address as _, AuthorizedFunction, MockAuth, MockAuthInvoke},
    token::{StellarAssetClient, TokenClient},
    Address, Env, IntoVal,
};

const FLOAT: i128 = 10_000_000;

// ── Setup ─────────────────────────────────────────────────────────────────────

fn setup() -> (Env, UptoAuthorizationClient<'static>, Address, Address) {
    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();

    let payer = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&payer, &FLOAT);

    let contract_id = env.register(UptoAuthorization, ());
    UptoAuthorizationClient::new(&env, &contract_id).initialize(&token);
    let client = UptoAuthorizationClient::new(&env, &contract_id);

    (env, client, payer, token)
}

// ── A. Recording-mode: auth tree inspection ───────────────────────────────────
//
// These tests use recording mode to capture the exact authorization tree
// Soroban creates. This directly answers: "Can a single signed auth entry
// contain the parent authorization plus nested approve sub-invocation?"

#[test]
fn test_1_recording_auth_tree_nested_approve() {
    let (env, client, payer, token) = setup();

    let payment_id: u32 = 1;
    let cap: i128 = 1_000_000;
    let seller = Address::generate(&env);

    // Execute: this triggers from.require_auth() + token.approve(from, ...)
    client.authorize(&payment_id, &payer, &seller, &cap, &100_000);

    // Capture the recorded authorization tree.
    let auths = env.auths();

    // Find payer's auth entry.
    let payer_auth = auths
        .iter()
        .find(|(addr, _)| *addr == payer)
        .expect("payer must have an auth entry in the recorded tree");

    let (_, payer_invocation) = payer_auth;

    // The payer's invocation must be: authorize(payment_id, from, to, cap, expiry)
    match &payer_invocation.function {
        AuthorizedFunction::Contract((addr, fn_name, _args)) => {
            assert_eq!(*addr, client.address);
            assert_eq!(fn_name.to_string(), "authorize");
        }
        other => panic!(
            "Expected Contract invocation, got: {:?}",
            std::mem::discriminant(other)
        ),
    }

    // CRITICAL: The payer's auth tree MUST contain the nested approve as a
    // sub-invocation. This is the tree structure that the payer signs.
    // If this works, it proves that one signed auth entry covers both calls.
    let has_nested_approve = payer_invocation.sub_invocations.iter().any(|sub| {
        matches!(
            &sub.function,
            AuthorizedFunction::Contract((addr, fn_name, _))
                if *addr == token && fn_name.to_string() == "approve"
        )
    });

    assert!(
        has_nested_approve,
        "The payer's auth tree MUST contain token.approve as a sub-invocation. \
         This proves the payer's single signature commits to the exact approve arguments."
    );

    // Verify token state: the nested approve executed.
    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.allowance(&payer, &client.address), cap);
}

#[test]
fn test_2_recording_full_flow_authorize_then_settle() {
    let (env, client, payer, token) = setup();

    let payment_id: u32 = 2;
    let cap: i128 = 2_500_000;
    let actual: i128 = 750_000;
    let seller = Address::generate(&env);

    // Phase 1: authorize
    client.authorize(&payment_id, &payer, &seller, &cap, &100_000);

    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.allowance(&payer, &client.address), cap);

    // Phase 2: settle
    client.settle(&payment_id, &actual);

    // Verify: allowance cleared, balances reflect transfer.
    assert_eq!(token_client.allowance(&payer, &client.address), 0);
    assert_eq!(token_client.balance(&payer), FLOAT - actual);
    assert_eq!(token_client.balance(&seller), actual);
}

#[test]
fn test_3_recording_auth_tree_structure_detail() {
    // This test inspects the EXACT structure of the auth tree to document
    // what the payer's signature commits to.
    let (env, client, payer, token) = setup();

    let payment_id: u32 = 3;
    let cap: i128 = 1_000_000;
    let seller = Address::generate(&env);

    client.authorize(&payment_id, &payer, &seller, &cap, &100_000);

    let auths = env.auths();
    let (_, payer_invocation) = auths
        .iter()
        .find(|(addr, _)| *addr == payer)
        .expect("payer auth entry must exist");

    // Print the full auth tree for documentation.
    println!("=== AUTH TREE STRUCTURE ===");
    println!("Payer: {payer:?}");
    println!("Root invocation: {:?}", payer_invocation.function);
    println!(
        "Sub-invocations count: {}",
        payer_invocation.sub_invocations.len()
    );
    for (i, sub) in payer_invocation.sub_invocations.iter().enumerate() {
        println!("  Sub[{i}]: {:?}", sub.function);
        for (j, nested) in sub.sub_invocations.iter().enumerate() {
            println!("    Nested[{j}]: {:?}", nested.function);
        }
    }
    println!("============================");

    // Verify: exactly 1 sub-invocation (the approve).
    assert_eq!(
        payer_invocation.sub_invocations.len(),
        1,
        "Expected exactly 1 sub-invocation (the token.approve)"
    );

    // Verify: the sub-invocation is token.approve.
    let sub = &payer_invocation.sub_invocations[0];
    match &sub.function {
        AuthorizedFunction::Contract((addr, fn_name, args)) => {
            assert_eq!(*addr, token);
            assert_eq!(fn_name.to_string(), "approve");
            // args: [from=payer, spender=contract, cap, expiry]
            assert_eq!(args.len(), 4);
        }
        other => panic!("Expected Contract sub-invocation, got: {:?}", other),
    }

    // Verify token state.
    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.allowance(&payer, &client.address), cap);
}

// ── B. Positive flow tests ────────────────────────────────────────────────────

#[test]
fn test_4_recording_double_settle_rejected() {
    let (env, client, payer, _token) = setup();

    let payment_id: u32 = 4;
    let cap: i128 = 1_000_000;
    let seller = Address::generate(&env);

    client.authorize(&payment_id, &payer, &seller, &cap, &100_000);
    client.settle(&payment_id, &500_000);

    let result = client.try_settle(&payment_id, &100);
    assert_eq!(result, Err(Ok(SpikeError::AlreadyConsumed)));
}

#[test]
fn test_5_recording_settle_exceeding_cap_rejected() {
    let (env, client, payer, _token) = setup();

    let payment_id: u32 = 5;
    let cap: i128 = 1_000_000;
    let seller = Address::generate(&env);

    client.authorize(&payment_id, &payer, &seller, &cap, &100_000);

    let result = client.try_settle(&payment_id, &(cap + 1));
    assert_eq!(result, Err(Ok(SpikeError::AmountExceedsCap)));
}

#[test]
fn test_6_recording_settle_without_authorize_rejected() {
    let (_env, client, _payer, _token) = setup();
    assert_eq!(
        client.try_settle(&999, &100),
        Err(Ok(SpikeError::NotSettled))
    );
}

#[test]
fn test_7_recording_zero_cap_rejected() {
    let (env, client, payer, _token) = setup();
    let seller = Address::generate(&env);
    assert_eq!(
        client.try_authorize(&999, &payer, &seller, &0, &100_000),
        Err(Ok(SpikeError::AmountExceedsCap))
    );
}

#[test]
fn test_8_recording_state_after_authorize() {
    let (env, client, payer, token) = setup();

    let payment_id: u32 = 80;
    let cap: i128 = 2_500_000;
    let seller = Address::generate(&env);

    client.authorize(&payment_id, &payer, &seller, &cap, &100_000);

    // Read the authorization record from contract storage.
    env.as_contract(&client.address, || {
        let record: AuthRecord = env.storage().persistent().get(&payment_id).unwrap();
        assert_eq!(record.from, payer);
        assert_eq!(record.to, seller);
        assert_eq!(record.cap, cap);
        assert_eq!(record.expiry, 100_000);
        assert!(!record.consumed);
    });

    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.allowance(&payer, &client.address), cap);
}

// ── C. Negative control: mock_auths with missing sub_invocation ───────────────
//
// These tests use `mock_auths()` (enforcing mode) to demonstrate that
// incomplete or mismatched auth trees are rejected by Soroban.

#[test]
fn test_9_mock_missing_sub_invocation_panics() {
    // If the auth tree does NOT include the nested approve as a sub-invocation,
    // Soroban MUST reject it. The payer cannot cover the nested approve
    // with just the parent auth entry.
    let (env, client, payer, _token) = setup();

    let payment_id: u32 = 9;
    let cap: i128 = 1_000_000;
    let seller = Address::generate(&env);
    let contract_addr = client.address.clone();

    let auth_args = soroban_sdk::vec![
        &env,
        payment_id.into_val(&env),
        payer.into_val(&env),
        seller.into_val(&env),
        cap.into_val(&env),
        100_000u32.into_val(&env),
    ];

    // Explicit auth entry WITHOUT sub_invokes — missing the nested approve.
    env.mock_auths(&[MockAuth {
        address: &payer,
        invoke: &MockAuthInvoke {
            contract: &contract_addr,
            fn_name: "authorize",
            args: auth_args,
            sub_invokes: &[], // <-- MISSING: no nested approve
        },
    }]);

    // Must panic: the nested approve has no auth coverage.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.authorize(&payment_id, &payer, &seller, &cap, &100_000);
    }));
    assert!(
        result.is_err(),
        "authorize MUST fail when the auth tree lacks the nested approve sub-invocation"
    );
}

#[test]
fn test_10_mock_wrong_approve_amount_panics() {
    // If the sub-invocation's approve amount doesn't match the actual call,
    // Soroban MUST reject it. The signed payload is immutable.
    let (env, client, payer, tok) = setup();

    let payment_id: u32 = 10;
    let cap: i128 = 1_000_000;
    let wrong_amount: i128 = 999_999;
    let seller = Address::generate(&env);
    let contract_addr = client.address.clone();

    let auth_args = soroban_sdk::vec![
        &env,
        payment_id.into_val(&env),
        payer.into_val(&env),
        seller.into_val(&env),
        cap.into_val(&env),
        100_000u32.into_val(&env),
    ];
    let approve_args = soroban_sdk::vec![
        &env,
        payer.into_val(&env),
        contract_addr.into_val(&env),
        wrong_amount.into_val(&env),
        100_000u32.into_val(&env),
    ];

    env.mock_auths(&[MockAuth {
        address: &payer,
        invoke: &MockAuthInvoke {
            contract: &contract_addr,
            fn_name: "authorize",
            args: auth_args,
            sub_invokes: &[MockAuthInvoke {
                contract: &tok,
                fn_name: "approve",
                args: approve_args,
                sub_invokes: &[],
            }],
        },
    }]);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.authorize(&payment_id, &payer, &seller, &cap, &100_000);
    }));
    assert!(
        result.is_err(),
        "authorize MUST fail when the sub-invocation approve amount is wrong"
    );
}

#[test]
fn test_11_mock_wrong_approve_spender_panics() {
    // If the sub-invocation's spender is wrong, Soroban MUST reject it.
    // The payer cannot redirect the approve to a different spender.
    let (env, client, payer, tok) = setup();

    let payment_id: u32 = 11;
    let cap: i128 = 1_000_000;
    let seller = Address::generate(&env);
    let contract_addr = client.address.clone();
    let wrong_spender = Address::generate(&env);

    let auth_args = soroban_sdk::vec![
        &env,
        payment_id.into_val(&env),
        payer.into_val(&env),
        seller.into_val(&env),
        cap.into_val(&env),
        100_000u32.into_val(&env),
    ];
    let approve_args = soroban_sdk::vec![
        &env,
        payer.into_val(&env),
        wrong_spender.into_val(&env),
        cap.into_val(&env),
        100_000u32.into_val(&env),
    ];

    env.mock_auths(&[MockAuth {
        address: &payer,
        invoke: &MockAuthInvoke {
            contract: &contract_addr,
            fn_name: "authorize",
            args: auth_args,
            sub_invokes: &[MockAuthInvoke {
                contract: &tok,
                fn_name: "approve",
                args: approve_args,
                sub_invokes: &[],
            }],
        },
    }]);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.authorize(&payment_id, &payer, &seller, &cap, &100_000);
    }));
    assert!(
        result.is_err(),
        "authorize MUST fail when the sub-invocation approve spender is wrong"
    );
}

// ── D. Budget measurements ────────────────────────────────────────────────────

#[test]
fn test_20_budget_nested_construction() {
    let (env, client, payer, _token) = setup();

    let payment_id: u32 = 200;
    let cap: i128 = 1_000_000;
    let seller = Address::generate(&env);

    // Nested: authorize calls token.approve as a sub-invocation.
    client.authorize(&payment_id, &payer, &seller, &cap, &100_000);

    let budget = env.cost_estimate().budget();
    let cpu = budget.cpu_instruction_cost();
    let mem = budget.memory_bytes_cost();

    println!("=== BUDGET: NESTED CONSTRUCTION ===");
    println!("  CPU instructions: {cpu}");
    println!("  Memory bytes:     {mem}");
    println!("  (authorize + nested token.approve)");
    println!("====================================");
}

#[test]
fn test_21_budget_separate_invocations() {
    let (env, client, payer, token) = setup();

    let payment_id: u32 = 210;
    let cap: i128 = 1_000_000;
    let seller = Address::generate(&env);

    // Separate: token.approve called directly, then authorize (no nested call).
    let token_client = TokenClient::new(&env, &token);
    token_client.approve(&payer, &client.address, &cap, &100_000);
    client.authorize(&payment_id, &payer, &seller, &cap, &100_000);

    let budget = env.cost_estimate().budget();
    let cpu = budget.cpu_instruction_cost();
    let mem = budget.memory_bytes_cost();

    println!("=== BUDGET: SEPARATE INVOCATIONS ===");
    println!("  CPU instructions: {cpu}");
    println!("  Memory bytes:     {mem}");
    println!("  (token.approve direct + authorize without nested)");
    println!("======================================");

    assert_eq!(token_client.allowance(&payer, &client.address), cap);
}
