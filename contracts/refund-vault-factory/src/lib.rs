#![no_std]

use accensa_common::Error;
use soroban_sdk::{
    contract, contractevent, contractimpl, contractmeta, contracttype, Address, BytesN, Env,
};

contractmeta!(key = "name", val = "RefundVaultFactory");
contractmeta!(key = "version", val = env!("CARGO_PKG_VERSION"));
contractmeta!(
    key = "repo",
    val = "https://github.com/accensa/accensa-contracts"
);
contractmeta!(key = "commit", val = env!("GIT_SHA"));

contractmeta!(key = "commit_dirty", val = env!("GIT_DIRTY"));

/// Approximately 30 days of ledgers, assuming ~5 seconds per ledger.
/// 60 * 60 * 24 * 30 / 5 = 518,400.
const TTL_EXTEND: u32 = 518_400;
/// The threshold before TTL is actually bumped, to prevent spamming updates on every call.
const TTL_THRESHOLD: u32 = 100;

#[contracttype]
pub enum DataKey {
    /// Factory admin; authorises every vault deployment.
    Admin,
    /// The installed `RefundVault` wasm hash. Every vault this factory deploys
    /// is created from this hash via `deploy_v2` (mirroring how
    /// `ReceiptAnchor` factory-deploys `ReceiptShard`s).
    VaultWasmHash,
    /// The default refund-policy contract address each generated vault is
    /// bound to unless `deploy_with_policy` overrides it.
    DefaultPolicy,
    /// Number of vaults deployed so far; the salt counter for the next one.
    VaultCount,
    /// Maps the sequential vault index to the deployed vault's address, so
    /// every vault the factory has produced is enumerable on-chain.
    Vault(u64),
}

/// Emitted by the factory every time it deploys a new vault.
///
/// Topics: `("vault_created_event", index)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultCreatedEvent {
    #[topic]
    pub index: u64,
    pub vault_address: Address,
    pub merchant: Address,
    pub token: Address,
    pub refund_window_ledgers: u32,
    pub policy: Address,
}

#[contract]
pub struct RefundVaultFactory;

/// A factory that deploys individual, lightweight `RefundVault` instances
/// (issue #129). The factory only ever creates vaults; each vault is an
/// independent contract with its own merchant, and is bound to a refund-policy
/// contract that it calls into on every refund.
///
/// The factory is the canonical deployment path: it stores the vault's wasm
/// hash once and `deploy_v2`s each instance from it, so callers never touch a
/// wasm file or CLI — they just invoke `deploy`/`deploy_with_policy` and
/// receive a live, already-`__constructor`-initialised vault address.
#[contractimpl]
impl RefundVaultFactory {
    /// Bind the factory to the `RefundVault` wasm it will deploy, the default
    /// refund-policy contract every generated vault is bound to, and an admin
    /// who authorises deployments. Called once by the deployment tooling.
    pub fn initialize(
        env: Env,
        admin: Address,
        vault_wasm_hash: BytesN<32>,
        default_policy: Address,
    ) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::VaultWasmHash, &vault_wasm_hash);
        env.storage()
            .instance()
            .set(&DataKey::DefaultPolicy, &default_policy);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    /// Deploy a new `RefundVault` bound to the factory's default policy.
    ///
    /// The vault is deployed via `__constructor(merchant, token,
    /// refund_window_ledgers, policy)` and returned fully initialised. Returns
    /// a per-deployment unique address; callers should record it (the factory
    /// also keeps a registry and emits [`VaultCreatedEvent`]).
    pub fn deploy(
        env: Env,
        merchant: Address,
        token: Address,
        refund_window_ledgers: u32,
    ) -> Result<Address, Error> {
        Self::authorize(&env)?;
        let policy: Address = env
            .storage()
            .instance()
            .get(&DataKey::DefaultPolicy)
            .ok_or(Error::NotInitialized)?;
        Self::deploy_vault(&env, merchant, token, refund_window_ledgers, policy)
    }

    /// Deploy a new `RefundVault` bound to an explicit policy contract, for
    /// vaults that must run a custom policy kind instead of the default.
    pub fn deploy_with_policy(
        env: Env,
        merchant: Address,
        token: Address,
        refund_window_ledgers: u32,
        policy: Address,
    ) -> Result<Address, Error> {
        Self::authorize(&env)?;
        Self::deploy_vault(&env, merchant, token, refund_window_ledgers, policy)
    }

    /// The deployed vault address for a sequential index, if any.
    pub fn get_vault(env: Env, index: u64) -> Option<Address> {
        env.storage().instance().get(&DataKey::Vault(index))
    }

    /// Total number of vaults this factory has deployed.
    pub fn get_vault_count(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::VaultCount)
            .unwrap_or(0)
    }

    /// The installed `RefundVault` wasm hash (set at `initialize`).
    pub fn get_vault_wasm_hash(env: Env) -> Option<BytesN<32>> {
        env.storage().instance().get(&DataKey::VaultWasmHash)
    }

    /// The default policy contract address (set at `initialize`).
    pub fn get_default_policy(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::DefaultPolicy)
    }

    fn authorize(env: &Env) -> Result<(), Error> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        admin.require_auth();
        Ok(())
    }

    /// Deploy a new vault from the installed wasm hash and record it in the
    /// on-chain registry. The salt is the sequential vault index, so deployed
    /// addresses are deterministic per deployment order but never collide.
    fn deploy_vault(
        env: &Env,
        merchant: Address,
        token: Address,
        refund_window_ledgers: u32,
        policy: Address,
    ) -> Result<Address, Error> {
        let wasm_hash: BytesN<32> = env
            .storage()
            .instance()
            .get(&DataKey::VaultWasmHash)
            .ok_or(Error::NotInitialized)?;

        let index: u64 = env
            .storage()
            .instance()
            .get(&DataKey::VaultCount)
            .unwrap_or(0);
        let salt = Self::vault_salt(env, index);

        let vault_addr = env.deployer().with_current_contract(salt).deploy_v2(
            wasm_hash,
            (
                merchant.clone(),
                token.clone(),
                refund_window_ledgers,
                policy.clone(),
            ),
        );

        env.storage()
            .instance()
            .set(&DataKey::Vault(index), &vault_addr);
        env.storage()
            .instance()
            .set(&DataKey::VaultCount, &(index + 1));

        VaultCreatedEvent {
            index,
            vault_address: vault_addr.clone(),
            merchant,
            token,
            refund_window_ledgers,
            policy,
        }
        .publish(env);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(vault_addr)
    }

    /// Deterministic per-index deploy salt: the vault index big-endian in the
    /// low 8 bytes, zero-padded (same scheme as `ReceiptAnchor::shard_salt`).
    fn vault_salt(env: &Env, index: u64) -> BytesN<32> {
        let mut bytes = [0u8; 32];
        bytes[24..32].copy_from_slice(&index.to_be_bytes());
        BytesN::from_array(env, &bytes)
    }
}

mod test;
