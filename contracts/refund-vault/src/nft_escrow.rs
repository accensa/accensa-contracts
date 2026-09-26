//! NFT escrow support for [`crate::RefundVault`] (issue #474).
//!
//! A vault's core float is a single SEP-41 fungible token, but payments can
//! be settled against collectibles too. This module escrows Soroban
//! non-fungible tokens **alongside** the fungible float so a merchant can
//! release an NFT to a buyer on the same lifecycle as a token refund.
//!
//! NFTs do not speak the SEP-41 fungible interface, so every interaction goes
//! through the parallel helpers in this module ([`nft_owner`],
//! [`nft_transfer`]) rather than [`soroban_sdk::token`]. The wire interface
//! used is the common Soroban non-fungible contract surface:
//!
//! - `owner(token_id: u128) -> Address`
//! - `transfer(from: Address, to: Address, token_id: u128)`, requiring
//!   `from`'s authorization
//!
//! An escrowed NFT is keyed by its exact `(nft_contract, token_id)` pair and
//! records the two parties. `token_id` is preserved end to end — `claim_nft`
//! and `refund_nft` return the very id they release, so the exact token that
//! was deposited is the exact token that comes back out.

use accensa_common::Error;
use soroban_sdk::{contractevent, contracttype, symbol_short, Address, Env, IntoVal, Val, Vec};

use crate::{acquire_reentrancy_lock, release_reentrancy_lock, DataKey};

/// The on-chain identity of an escrowed NFT.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NftEscrowRecord {
    /// The merchant (vault admin) who escrowed the NFT and may reclaim it via
    /// [`crate::RefundVault::claim_nft`].
    pub merchant: Address,
    /// The buyer the NFT is being escrowed for; the only address that may
    /// redeem it via [`crate::RefundVault::refund_nft`].
    pub buyer: Address,
}

/// Emitted when the merchant escrows an NFT into the vault.
///
/// Topics: `("nft_deposited_event", nft_contract, token_id)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NftDepositedEvent {
    #[topic]
    pub nft_contract: Address,
    #[topic]
    pub token_id: u128,
    pub merchant: Address,
    pub buyer: Address,
}

/// Emitted when the merchant reclaims an escrowed NFT (cancellation / return).
///
/// Topics: `("nft_claimed_event", nft_contract, token_id)`. The exact
/// `token_id` is echoed back to the escrow owner.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NftClaimedEvent {
    #[topic]
    pub nft_contract: Address,
    #[topic]
    pub token_id: u128,
    pub merchant: Address,
}

/// Emitted when an escrowed NFT is refunded to the buyer.
///
/// Topics: `("nft_refunded_event", nft_contract, token_id)`. The exact
/// `token_id` is echoed back to the escrow owner.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NftRefundedEvent {
    #[topic]
    pub nft_contract: Address,
    #[topic]
    pub token_id: u128,
    pub buyer: Address,
}

/// Query the current owner of `token_id` on an NFT contract.
pub(crate) fn nft_owner(env: &Env, contract: &Address, token_id: u128) -> Address {
    let args = Vec::from_array(env, [token_id.into_val(env)]);
    env.invoke_contract(contract, &symbol_short!("owner"), args)
}

/// Transfer `token_id` from `from` to `to` on an NFT contract. The NFT
/// contract requires `from`'s authorization; under an authenticated outer
/// invocation the required signatures are provided by the calling party.
pub(crate) fn nft_transfer(
    env: &Env,
    contract: &Address,
    from: &Address,
    to: &Address,
    token_id: u128,
) {
    let args = Vec::from_array(
        env,
        [
            from.clone().into_val(env),
            to.clone().into_val(env),
            token_id.into_val(env),
        ],
    );
    let _: Val = env.invoke_contract(contract, &symbol_short!("transfer"), args);
}

fn escrow_key(contract: &Address, token_id: u128) -> DataKey {
    DataKey::NftEscrow(contract.clone(), token_id)
}

fn is_paused(env: &Env) -> bool {
    env.storage()
        .instance()
        .get(&DataKey::IsPaused)
        .unwrap_or(false)
}

fn get_record(env: &Env, contract: &Address, token_id: u128) -> Option<NftEscrowRecord> {
    env.storage()
        .persistent()
        .get(&escrow_key(contract, token_id))
}

/// Shared release path: verify the vault still owns the exact token, hand it
/// to `recipient` on the NFT contract, and delete the escrow record. Returns
/// the released `token_id`.
fn release(
    env: &Env,
    contract: &Address,
    token_id: u128,
    recipient: &Address,
) -> Result<u128, Error> {
    let vault = env.current_contract_address();
    if recipient == &vault {
        return Err(Error::SelfTransfer);
    }
    if nft_owner(env, contract, token_id) != vault {
        return Err(Error::NftNotOwned);
    }
    nft_transfer(env, contract, &vault, recipient, token_id);
    env.storage()
        .persistent()
        .remove(&escrow_key(contract, token_id));
    Ok(token_id)
}

pub(crate) fn deposit(
    env: &Env,
    merchant: &Address,
    buyer: &Address,
    contract: &Address,
    token_id: u128,
) -> Result<(), Error> {
    acquire_reentrancy_lock(env)?;
    if is_paused(env) {
        return Err(Error::Paused);
    }

    let admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(Error::NotInitialized)?;
    if merchant != &admin {
        return Err(Error::Unauthorized);
    }
    merchant.require_auth();

    if get_record(env, contract, token_id).is_some() {
        return Err(Error::NftAlreadyEscrowed);
    }

    // Pull the NFT from the merchant into the vault. `from == merchant` on
    // the NFT contract requires the merchant's authorization, which is
    // supplied as part of this authenticated invocation.
    nft_transfer(
        env,
        contract,
        merchant,
        &env.current_contract_address(),
        token_id,
    );

    env.storage().persistent().set(
        &escrow_key(contract, token_id),
        &NftEscrowRecord {
            merchant: admin,
            buyer: buyer.clone(),
        },
    );

    NftDepositedEvent {
        nft_contract: contract.clone(),
        token_id,
        merchant: merchant.clone(),
        buyer: buyer.clone(),
    }
    .publish(env);

    release_reentrancy_lock(env);
    Ok(())
}

pub(crate) fn claim(
    env: &Env,
    merchant: &Address,
    contract: &Address,
    token_id: u128,
) -> Result<u128, Error> {
    acquire_reentrancy_lock(env)?;
    if is_paused(env) {
        return Err(Error::Paused);
    }

    let record = get_record(env, contract, token_id).ok_or(Error::NftEscrowNotFound)?;
    if &record.merchant != merchant {
        return Err(Error::Unauthorized);
    }
    merchant.require_auth();

    release(env, contract, token_id, merchant)?;

    NftClaimedEvent {
        nft_contract: contract.clone(),
        token_id,
        merchant: merchant.clone(),
    }
    .publish(env);

    release_reentrancy_lock(env);
    Ok(token_id)
}

pub(crate) fn refund(
    env: &Env,
    buyer: &Address,
    contract: &Address,
    token_id: u128,
) -> Result<u128, Error> {
    acquire_reentrancy_lock(env)?;
    if is_paused(env) {
        return Err(Error::Paused);
    }

    let record = get_record(env, contract, token_id).ok_or(Error::NftEscrowNotFound)?;
    if &record.buyer != buyer {
        return Err(Error::Unauthorized);
    }
    buyer.require_auth();

    release(env, contract, token_id, buyer)?;

    NftRefundedEvent {
        nft_contract: contract.clone(),
        token_id,
        buyer: buyer.clone(),
    }
    .publish(env);

    release_reentrancy_lock(env);
    Ok(token_id)
}

pub(crate) fn get(env: &Env, contract: &Address, token_id: u128) -> Option<NftEscrowRecord> {
    get_record(env, contract, token_id)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use soroban_sdk::{
        contract, contractimpl, contracttype, testutils::Address as _, Address, Env,
    };

    use crate::{RefundVault, RefundVaultClient, VaultInit};

    /// Minimal Soroban non-fungible contract implementing the wire surface
    /// [`nft_owner`] / [`nft_transfer`] rely on.
    #[contract]
    struct MockNft;

    #[contracttype]
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum NftKey {
        Admin,
        Owner(u128),
    }

    #[contractimpl]
    impl MockNft {
        pub fn init(env: Env, admin: Address) {
            env.storage().instance().set(&NftKey::Admin, &admin);
        }

        pub fn mint(env: Env, to: Address, token_id: u128) {
            let admin: Address = env.storage().instance().get(&NftKey::Admin).unwrap();
            admin.require_auth();
            env.storage()
                .persistent()
                .set(&NftKey::Owner(token_id), &to);
        }

        pub fn owner(env: Env, token_id: u128) -> Address {
            let zero = Address::from_string(&soroban_sdk::String::from_str(
                &env,
                "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
            ));
            env.storage()
                .persistent()
                .get(&NftKey::Owner(token_id))
                .unwrap_or_else(|| zero.clone())
        }

        pub fn transfer(env: Env, from: Address, to: Address, token_id: u128) {
            from.require_auth();
            let zero = Address::from_string(&soroban_sdk::String::from_str(
                &env,
                "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
            ));
            let owner: Address = env
                .storage()
                .persistent()
                .get(&NftKey::Owner(token_id))
                .unwrap_or_else(|| zero.clone());
            assert!(
                owner == from,
                "only the current owner may transfer the token"
            );
            env.storage()
                .persistent()
                .set(&NftKey::Owner(token_id), &to);
        }
    }

    struct Harness {
        env: Env,
        vault: RefundVaultClient<'static>,
        vault_id: Address,
        nft: Address,
        merchant: Address,
        buyer: Address,
    }

    fn setup() -> Harness {
        let env = Env::default();
        env.mock_all_auths();

        let merchant = Address::generate(&env);
        let buyer = Address::generate(&env);

        let token = env
            .register_stellar_asset_contract_v2(merchant.clone())
            .address();

        let init = VaultInit {
            merchant: merchant.clone(),
            token,
            time_policy: None,
            vdf_policy: None,
            fee_bps: 0,
            fee_recipient: None,
            refund_window: 0,
            deadline: 0,
            vdf_delay: 0,
        };
        let vault_id = env.register(RefundVault, (init,));
        let vault = RefundVaultClient::new(&env, &vault_id);

        let nft_id = env.register(MockNft, ());
        MockNftClient::new(&env, &nft_id).init(&merchant);

        Harness {
            env,
            vault,
            vault_id,
            nft: nft_id,
            merchant,
            buyer,
        }
    }

    fn mint_and_deposit(h: &Harness, token_id: u128) {
        MockNftClient::new(&h.env, &h.nft).mint(&h.merchant, &token_id);
        h.vault
            .deposit_nft(&h.merchant, &h.buyer, &h.nft, &token_id);
    }

    fn holder(env: &Env, nft: &Address, token_id: u128) -> Address {
        MockNftClient::new(env, nft).owner(&token_id)
    }

    #[test]
    fn nft_deposit_escrows_to_the_vault() {
        let h = setup();
        mint_and_deposit(&h, 7);

        assert_eq!(holder(&h.env, &h.nft, 7), h.vault_id);
        assert!(h.vault.get_nft_escrow(&h.nft, &7).is_some());
    }

    #[test]
    fn nft_deposit_rejects_double_escrow() {
        let h = setup();
        mint_and_deposit(&h, 7);

        assert_eq!(
            h.vault.try_deposit_nft(&h.merchant, &h.buyer, &h.nft, &7),
            Err(Ok(accensa_common::Error::NftAlreadyEscrowed))
        );
    }

    #[test]
    fn nft_deposit_requires_the_merchant() {
        let h = setup();
        MockNftClient::new(&h.env, &h.nft).mint(&h.merchant, &7);
        let outsider = Address::generate(&h.env);

        assert_eq!(
            h.vault.try_deposit_nft(&outsider, &h.buyer, &h.nft, &7),
            Err(Ok(accensa_common::Error::Unauthorized))
        );
    }

    #[test]
    fn merchant_claim_returns_the_exact_token_id() {
        let h = setup();
        mint_and_deposit(&h, 42);

        let returned = h.vault.claim_nft(&h.merchant, &h.nft, &42);
        assert_eq!(returned, 42, "claim must return the exact token id");
        assert_eq!(holder(&h.env, &h.nft, 42), h.merchant);
        assert!(!h.vault.get_nft_escrow(&h.nft, &42).is_some());
    }

    #[test]
    fn buyer_refund_returns_the_exact_token_id() {
        let h = setup();
        mint_and_deposit(&h, 99);

        let returned = h.vault.refund_nft(&h.buyer, &h.nft, &99);
        assert_eq!(returned, 99, "refund must return the exact token id");
        assert_eq!(holder(&h.env, &h.nft, 99), h.buyer);
        assert!(!h.vault.get_nft_escrow(&h.nft, &99).is_some());
    }

    #[test]
    fn refund_reverts_for_the_wrong_party() {
        let h = setup();
        mint_and_deposit(&h, 5);
        let stranger = Address::generate(&h.env);

        assert_eq!(
            h.vault.try_refund_nft(&stranger, &h.nft, &5),
            Err(Ok(accensa_common::Error::Unauthorized))
        );
        assert!(h.vault.get_nft_escrow(&h.nft, &5).is_some());
        assert_eq!(holder(&h.env, &h.nft, 5), h.vault_id);
    }

    #[test]
    fn claim_reverts_when_nft_not_escrowed_or_not_owned() {
        let h = setup();
        mint_and_deposit(&h, 3);

        assert_eq!(
            h.vault.try_claim_nft(&h.merchant, &h.nft, &1234),
            Err(Ok(accensa_common::Error::NftEscrowNotFound))
        );

        // Transfer the NFT out from under the vault, then claim must detect
        // the vault no longer owns it.
        let nft = MockNftClient::new(&h.env, &h.nft);
        nft.transfer(&h.vault_id, &Address::generate(&h.env), &3);
        assert_eq!(
            h.vault.try_claim_nft(&h.merchant, &h.nft, &3),
            Err(Ok(accensa_common::Error::NftNotOwned))
        );
    }

    #[test]
    fn nft_full_lifecycle_preserves_the_exact_token_id() {
        let h = setup();
        let token_id: u128 = 1_000_000;

        mint_and_deposit(&h, token_id);
        assert_eq!(holder(&h.env, &h.nft, token_id), h.vault_id);

        // Refund: the very same token id moves to the buyer.
        let refunded = h.vault.refund_nft(&h.buyer, &h.nft, &token_id);
        assert_eq!(refunded, token_id);
        assert_eq!(holder(&h.env, &h.nft, token_id), h.buyer);
        assert!(!h.vault.get_nft_escrow(&h.nft, &token_id).is_some());

        // A fresh escrow of the same id, claimed back to the merchant, must
        // still carry that exact id through.
        MockNftClient::new(&h.env, &h.nft).mint(&h.merchant, &token_id);
        h.vault
            .deposit_nft(&h.merchant, &h.buyer, &h.nft, &token_id);
        let claimed = h.vault.claim_nft(&h.merchant, &h.nft, &token_id);
        assert_eq!(claimed, token_id);
        assert_eq!(holder(&h.env, &h.nft, token_id), h.merchant);
    }
}
