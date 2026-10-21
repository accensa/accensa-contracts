//! Permit2-style gasless deposit approvals (issue #478).
//!
//! Integrates an off-chain permit signature system allowing depositors to authorize
//! vault deposits off-chain, enabling merchants or relayers to submit the transaction
//! and pay the gas fees:
//!
//! - [`DepositPermit`] encapsulates the depositor, amount, user nonce, and deadline.
//! - [`permit_digest`] binds the permit tuple to the vault's instance-unique
//!   [`DataKey::DomainSeparator`] to prevent cross-deployment or cross-operation replay.
//! - [`deposit_with_permit`] verifies the Ed25519 signature, consumes the user's
//!   monotonic [`DataKey::UserNonce`], transfers the funds into the vault float,
//!   and emits [`PermitDepositEvent`].

use accensa_common::{storage::extend_instance_ttl, Error};
use soroban_sdk::{contractevent, contracttype, token, xdr::ToXdr, Address, Bytes, BytesN, Env};

use crate::DataKey;

/// Payload signed by the depositor to authorize a gasless deposit.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DepositPermit {
    /// The depositor whose funds will be transferred to the vault.
    pub from: Address,
    /// Amount of tokens to deposit.
    pub amount: i128,
    /// User nonce to prevent signature replay (tracks `DataKey::UserNonce`).
    pub nonce: u64,
    /// Unix timestamp deadline after which the permit is invalid (0 = no expiry).
    pub deadline: u64,
}

/// Emitted when a gasless permit deposit is successfully processed.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermitDepositEvent {
    #[topic]
    pub relayer: Address,
    #[topic]
    pub from: Address,
    pub amount: i128,
    pub nonce: u64,
}

/// Computes the canonical message digest for a [`DepositPermit`].
pub fn permit_digest(
    env: &Env,
    domain_separator: &BytesN<32>,
    permit: &DepositPermit,
) -> BytesN<32> {
    let mut payload = Bytes::from_slice(env, b"accensa-vault-permit-deposit-v1");
    payload.append(&Bytes::from_slice(
        env,
        domain_separator.to_array().as_slice(),
    ));
    payload.append(&permit.from.clone().to_xdr(env));
    payload.extend_from_slice(&permit.amount.to_be_bytes());
    payload.extend_from_slice(&permit.nonce.to_be_bytes());
    payload.extend_from_slice(&permit.deadline.to_be_bytes());
    env.crypto().sha256(&payload).into()
}

/// Executes a gasless vault deposit authorized by a depositor's permit signature.
///
/// The `relayer` provides transaction gas and authorises the top-level call.
pub fn deposit_with_permit(
    env: &Env,
    relayer: Address,
    permit: DepositPermit,
    signer_pubkey: BytesN<32>,
    signature: BytesN<64>,
) -> Result<(), Error> {
    accensa_common::reentrancy::ReentrancyGuard::acquire(env)?;

    let result = (|| -> Result<(), Error> {
        if env
            .storage()
            .instance()
            .get(&DataKey::IsPaused)
            .unwrap_or(false)
        {
            return Err(Error::Paused);
        }

        relayer.require_auth();

        if permit.amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let now = env.ledger().timestamp();
        if permit.deadline > 0 && now > permit.deadline {
            return Err(Error::WindowExpired);
        }

        let domain_separator: BytesN<32> = env
            .storage()
            .instance()
            .get(&DataKey::DomainSeparator)
            .ok_or(Error::NotInitialized)?;

        // Monotonic user nonce replay protection
        let nonce_key = DataKey::UserNonce(permit.from.clone());
        let expected_nonce: u64 = env.storage().persistent().get(&nonce_key).unwrap_or(0);
        if permit.nonce != expected_nonce {
            return Err(Error::StaleState);
        }

        // Verify cryptographic signature over domain-separated permit
        let digest = permit_digest(env, &domain_separator, &permit);
        let msg: Bytes = digest.into();
        env.crypto()
            .ed25519_verify(&signer_pubkey, &msg, &signature);

        env.storage()
            .persistent()
            .set(&nonce_key, &(expected_nonce + 1));

        let token_address: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(Error::NotInitialized)?;
        let token_client = token::Client::new(env, &token_address);
        let contract_addr = env.current_contract_address();

        token_client.transfer_from(&contract_addr, &permit.from, &contract_addr, &permit.amount);

        PermitDepositEvent {
            relayer,
            from: permit.from,
            amount: permit.amount,
            nonce: permit.nonce,
        }
        .publish(env);

        extend_instance_ttl(env, crate::TTL_THRESHOLD, crate::TTL_EXTEND);
        Ok(())
    })();

    accensa_common::reentrancy::ReentrancyGuard::release(env);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::vault_init;
    use soroban_sdk::{
        testutils::{Address as _, Ledger as _},
        token::StellarAssetClient,
    };

    #[allow(dead_code)]
    struct TestHarness {
        env: Env,
        contract_id: Address,
        merchant: Address,
        token: Address,
    }

    fn setup() -> TestHarness {
        let env = Env::default();
        env.mock_all_auths();

        let merchant = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let sac = env.register_stellar_asset_contract_v2(token_admin);
        let token = sac.address();

        let contract_id = env.register(
            crate::RefundVault,
            (vault_init(&env, &merchant, &token, 100),),
        );

        TestHarness {
            env,
            contract_id,
            merchant,
            token,
        }
    }

    const SIGNER_PUBKEY: [u8; 32] = [
        228, 66, 244, 182, 253, 131, 57, 239, 238, 247, 75, 211, 35, 145, 35, 252, 206, 244, 170,
        166, 162, 103, 21, 12, 111, 176, 41, 204, 184, 35, 250, 49,
    ];

    const VALID_SIG: [u8; 64] = [
        36, 123, 147, 183, 239, 197, 75, 104, 158, 56, 80, 56, 184, 89, 185, 22, 123, 57, 167, 142,
        97, 82, 243, 85, 187, 183, 74, 119, 101, 201, 83, 248, 3, 170, 89, 162, 57, 117, 55, 161,
        61, 114, 131, 33, 118, 176, 9, 80, 85, 39, 107, 53, 185, 254, 36, 30, 25, 242, 197, 194,
        247, 29, 179, 6,
    ];

    #[test]
    fn test_deposit_with_permit_happy_path_and_replay_protection() {
        let h = setup();
        let relayer = Address::generate(&h.env);
        let depositor = Address::generate(&h.env);

        StellarAssetClient::new(&h.env, &h.token).mint(&depositor, &100_000);
        token::Client::new(&h.env, &h.token).approve(&depositor, &h.contract_id, &100_000, &10_000);

        let permit = DepositPermit {
            from: depositor.clone(),
            amount: 25_000,
            nonce: 0,
            deadline: 1000,
        };

        let signer_pubkey = BytesN::from_array(&h.env, &SIGNER_PUBKEY);
        let signature = BytesN::from_array(&h.env, &VALID_SIG);

        // Relayer submits the permit deposit
        h.env.as_contract(&h.contract_id, || {
            deposit_with_permit(
                &h.env,
                relayer.clone(),
                permit.clone(),
                signer_pubkey.clone(),
                signature.clone(),
            )
            .unwrap();

            // Verify nonce incremented
            let nonce: u64 = h
                .env
                .storage()
                .persistent()
                .get(&DataKey::UserNonce(depositor.clone()))
                .unwrap();
            assert_eq!(nonce, 1);
        });

        let token_client = token::Client::new(&h.env, &h.token);
        assert_eq!(token_client.balance(&h.contract_id), 25_000);
        assert_eq!(token_client.balance(&depositor), 75_000);

        // Replaying the exact same permit fails with StaleState
        h.env.as_contract(&h.contract_id, || {
            let err = deposit_with_permit(&h.env, relayer, permit, signer_pubkey, signature);
            assert_eq!(err, Err(Error::StaleState));
        });
    }

    #[test]
    fn test_deposit_with_permit_expired_deadline_fails() {
        let h = setup();
        let relayer = Address::generate(&h.env);
        let depositor = Address::generate(&h.env);

        let permit = DepositPermit {
            from: depositor,
            amount: 10_000,
            nonce: 0,
            deadline: 0, // will be tested below
        };

        // When deadline is before current timestamp
        let expired_permit = DepositPermit {
            deadline: 5,
            ..permit
        };
        h.env.ledger().with_mut(|li| li.timestamp = 10);

        let signer_pubkey = BytesN::from_array(&h.env, &SIGNER_PUBKEY);
        let signature = BytesN::from_array(&h.env, &VALID_SIG);

        h.env.as_contract(&h.contract_id, || {
            let err =
                deposit_with_permit(&h.env, relayer, expired_permit, signer_pubkey, signature);
            assert_eq!(err, Err(Error::WindowExpired));
        });
    }

    #[test]
    fn test_deposit_with_permit_invalid_amount_fails() {
        let h = setup();
        let relayer = Address::generate(&h.env);
        let depositor = Address::generate(&h.env);

        let permit_zero = DepositPermit {
            from: depositor.clone(),
            amount: 0,
            nonce: 0,
            deadline: 1000,
        };
        let permit_neg = DepositPermit {
            from: depositor,
            amount: -100,
            nonce: 0,
            deadline: 1000,
        };

        let signer_pubkey = BytesN::from_array(&h.env, &SIGNER_PUBKEY);
        let signature = BytesN::from_array(&h.env, &VALID_SIG);

        h.env.as_contract(&h.contract_id, || {
            assert_eq!(
                deposit_with_permit(
                    &h.env,
                    relayer,
                    permit_zero,
                    signer_pubkey.clone(),
                    signature.clone()
                ),
                Err(Error::InvalidAmount)
            );
        });

        let relayer2 = Address::generate(&h.env);
        h.env.as_contract(&h.contract_id, || {
            assert_eq!(
                deposit_with_permit(&h.env, relayer2, permit_neg, signer_pubkey, signature),
                Err(Error::InvalidAmount)
            );
        });
    }

    #[test]
    fn test_deposit_with_permit_paused_fails() {
        let h = setup();
        let relayer = Address::generate(&h.env);
        let depositor = Address::generate(&h.env);

        let permit = DepositPermit {
            from: depositor,
            amount: 10_000,
            nonce: 0,
            deadline: 1000,
        };

        let signer_pubkey = BytesN::from_array(&h.env, &SIGNER_PUBKEY);
        let signature = BytesN::from_array(&h.env, &VALID_SIG);

        h.env.as_contract(&h.contract_id, || {
            h.env.storage().instance().set(&DataKey::IsPaused, &true);
            let err = deposit_with_permit(&h.env, relayer, permit, signer_pubkey, signature);
            assert_eq!(err, Err(Error::Paused));
        });
    }

    #[test]
    #[should_panic]
    fn test_deposit_with_permit_invalid_signature_traps() {
        let h = setup();
        let relayer = Address::generate(&h.env);
        let depositor = Address::generate(&h.env);

        let permit = DepositPermit {
            from: depositor,
            amount: 25_000,
            nonce: 0,
            deadline: 1000,
        };

        let signer_pubkey = BytesN::from_array(&h.env, &SIGNER_PUBKEY);
        let mut corrupted_sig = VALID_SIG;
        corrupted_sig[0] ^= 0xff;
        let signature = BytesN::from_array(&h.env, &corrupted_sig);

        h.env.as_contract(&h.contract_id, || {
            let _ = deposit_with_permit(&h.env, relayer, permit, signer_pubkey, signature);
        });
    }
}
