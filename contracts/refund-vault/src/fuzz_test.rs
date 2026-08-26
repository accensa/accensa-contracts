#![cfg(test)]

use crate::{Error, RefundVault, RefundVaultClient};
use proptest::prelude::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env,
};

const FLOAT: i128 = 1_000_000_000_000;

fn setup(window: u32) -> (Env, RefundVaultClient<'static>, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let contract_id = env.register(RefundVault, ());
    let client = RefundVaultClient::new(&env, &contract_id);
    client.initialize(&merchant, &token, &window);

    (env, client, merchant, token)
}

proptest! {
    #[test]
    fn test_fuzz_deposit_extreme_amounts(
        amount in proptest::num::i128::ANY
    ) {
        let (_, client, merchant, _) = setup(100);
        let res = client.try_deposit(&merchant, &amount);
        if amount <= 0 {
            assert_eq!(res, Err(Ok(Error::InvalidAmount)));
        } else if amount > FLOAT {
            // It will panic in the token contract
            assert!(res.is_err());
        } else {
            assert!(res.is_ok());
        }
    }

    #[test]
    fn test_fuzz_ttl_extension(
        ledger in 1u32..1_000_000u32
    ) {
        let (env, client, _, _) = setup(100);
        env.ledger().set_sequence_number(ledger);

        let payment_ref =
            BytesN::from_array(&env, &[0; 32]);
        let res = client.try_extend_refund_ttl(
            &payment_ref,
        );
        assert_eq!(res, Err(Ok(Error::RefundNotFound)));
    }

    #[test]
    fn test_fuzz_refund_i128_boundaries(
        amount in prop_oneof![
            Just(0i128),
            Just(1i128),
            Just(-1i128),
            Just(i128::MIN),
            Just(i128::MIN + 1),
            Just(i128::MAX),
            Just(i128::MAX - 1),
            proptest::num::i128::ANY,
        ]
    ) {
        let (env, client, merchant, _token) =
            setup(100);
        client.deposit(&merchant, &100);

        let payment_ref =
            BytesN::from_array(&env, &[0u8; 32]);
        let buyer = Address::generate(&env);
        let res = client.try_refund(
            &payment_ref, &buyer, &amount, &0,
        );

        if amount <= 0 {
            assert_eq!(
                res, Err(Ok(Error::InvalidAmount))
            );
        } else if amount > 100 {
            assert_eq!(
                res,
                Err(Ok(Error::InsufficientFloat))
            );
        } else {
            assert!(res.is_ok());
        }
    }

    #[test]
    fn test_fuzz_deposit_i128_boundaries(
        amount in prop_oneof![
            Just(0i128),
            Just(1i128),
            Just(-1i128),
            Just(i128::MIN),
            Just(i128::MIN + 1),
            Just(i128::MAX),
            Just(i128::MAX - 1),
            proptest::num::i128::ANY,
        ]
    ) {
        let (_, client, merchant, _) = setup(100);
        let res = client.try_deposit(
            &merchant, &amount,
        );

        if amount <= 0 {
            assert_eq!(
                res, Err(Ok(Error::InvalidAmount))
            );
        } else if amount > FLOAT {
            assert!(res.is_err());
        } else {
            assert!(res.is_ok());
        }
    }
}

// ── Accounting invariant fuzz test ─────────────────────────────────────────

#[derive(Debug, Clone)]
enum VaultOp {
    Deposit(i128),
    Refund(i128),
    Withdraw(i128),
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    #[test]
    fn test_fuzz_vault_accounting_invariant(
        ops in prop::collection::vec(
            prop_oneof![
                (1i128..100_000).prop_map(VaultOp::Deposit),
                (1i128..1_000).prop_map(VaultOp::Refund),
                (1i128..1_000).prop_map(VaultOp::Withdraw),
            ],
            0..30,
        )
    ) {
        let (env, client, merchant, token) =
            setup(100_000_000);
        let token_client = TokenClient::new(
            &env, &token,
        );

        let mut total_deposits: i128 = 0;
        let mut total_refunds: i128 = 0;
        let mut total_withdrawals: i128 = 0;
        let mut refund_counter: u32 = 0;

        for op in ops {
            match op {
                VaultOp::Deposit(amount) => {
                    if token_client.balance(&merchant)
                        >= amount
                    {
                        if client
                            .try_deposit(
                                &merchant, &amount,
                            )
                            .is_ok()
                        {
                            total_deposits += amount;
                        }
                    }
                }
                VaultOp::Refund(amount) => {
                    let mut pr = [0u8; 32];
                    pr[..4].copy_from_slice(
                        &refund_counter.to_le_bytes(),
                    );
                    refund_counter = refund_counter
                        .wrapping_add(1);
                    let payment_ref =
                        BytesN::from_array(&env, &pr);
                    let buyer =
                        Address::generate(&env);
                    if client
                        .try_refund(
                            &payment_ref,
                            &buyer,
                            &amount,
                            &0,
                        )
                        .is_ok()
                    {
                        total_refunds += amount;
                    }
                }
                VaultOp::Withdraw(amount) => {
                    if client
                        .try_withdraw(
                            &amount, &merchant,
                        )
                        .is_ok()
                    {
                        total_withdrawals += amount;
                    }
                }
            }
        }

        let vault_balance = token_client
            .balance(&client.address);

        // Invariant 1: vault float is non-negative.
        prop_assert!(
            vault_balance >= 0,
            "vault balance must be >= 0, got {}",
            vault_balance,
        );

        // Invariant 2: without yield, vault balance
        // equals net flow through the contract.
        prop_assert_eq!(
            vault_balance,
            total_deposits
                - total_refunds
                - total_withdrawals,
            "vault balance ({}) must equal \
             deposits ({}) - refunds ({}) \
             - withdrawals ({})",
            vault_balance,
            total_deposits,
            total_refunds,
            total_withdrawals,
        );
    }
}
