//! Deploys lightweight [`accensa_common::VaultInit`]-configured vault
//! instances for many merchants off a single factory (issue #129).
//!
//! The factory owns the deployment inputs a merchant should not be able to
//! pick at will — the vault `wasm_hash` and the addresses of the stateless
//! policy contracts — and binds each `deploy_vault` to the merchant via
//! [`soroban_sdk::Address::require_auth`]. Addresses are deterministic: a salt
//! is derived from the merchant and a monotonically increasing counter, so a
//! given merchant always lands on an address within a fixed salt family.
//!
//! Policy-resolution rule: a nonzero field in the merchant-supplied
//! [`accensa_common::VaultInit`] wins; `None` falls back to the factory's
//! global policy addresses (set at initialization, rotatable by the admin).
//! A vault with a `None` policy on a nonzero gate is deployed but refuses
//! that gate at claim time with
//! [`accensa_common::Error::PolicyContractsNotConfigured`]; the factory
//! operator's job is to never let that happen.

#![no_std]

use accensa_common::VaultInit;
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, symbol_short, xdr::ToXdr, Address,
    BytesN, Env, Symbol, Vec,
};

pub(crate) const KEY_ADMIN: Symbol = symbol_short!("admin");
pub(crate) const KEY_VAULT_WASM: Symbol = symbol_short!("vwasm");
pub(crate) const KEY_TIME: Symbol = symbol_short!("time");
pub(crate) const KEY_VDF: Symbol = symbol_short!("vdf");
pub(crate) const KEY_VAULTS: Symbol = symbol_short!("vaults");
pub(crate) const KEY_NEXT_SALT: Symbol = symbol_short!("nsalt");
pub(crate) const KEY_INITED: Symbol = symbol_short!("inited");

/// Emitted with the merchant as a topic when `deploy_vault` mints a new vault
/// instance.
#[contractevent]
pub struct VaultDeployedEvent {
    #[topic]
    pub merchant: Address,
    pub vault: Address,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    /// `initialize` / `__constructor` was called more than once.
    AlreadyInitialized = 1,
    /// The factory was never initialized.
    NotInitialized = 2,
    /// The caller is not the factory admin.
    NotAuthorized = 3,
    /// `set_vault_wasm` cannot clear the hash (only replace it).
    InvalidWasmHash = 4,
    /// The merchant attempted to deploy with a pre-emptive salt collision.
    SaltCollision = 5,
}

#[contract]
pub struct RefundVaultFactory;

#[contractimpl]
impl RefundVaultFactory {
    /// Constructor (factory-wired deployments): records the admin, the vault
    /// `wasm_hash` the factory may deploy, and the default policy addresses.
    pub fn __constructor(
        env: Env,
        admin: Address,
        vault_wasm_hash: BytesN<32>,
        time_policy: Option<Address>,
        vdf_policy: Option<Address>,
    ) -> Result<(), Error> {
        init(&env, admin, vault_wasm_hash, time_policy, vdf_policy)
    }

    /// `initialize` alias of [`__constructor`](Self::__constructor) for
    /// environments where deploy-via-constructor is unavailable.
    pub fn initialize(
        env: Env,
        admin: Address,
        vault_wasm_hash: BytesN<32>,
        time_policy: Option<Address>,
        vdf_policy: Option<Address>,
    ) -> Result<(), Error> {
        init(&env, admin, vault_wasm_hash, time_policy, vdf_policy)
    }

    /// Deploys a vault instance configured by `init`. Requires the merchant's
    /// authorization (griefing cannot be engineered on someone else's salt
    /// family). Returns the new vault's address deterministically.
    pub fn deploy_vault(env: Env, init: VaultInit) -> Result<Address, Error> {
        require_initialized(&env)?;
        init.merchant.require_auth();

        let time_policy = init
            .time_policy
            .clone()
            .or_else(|| env.storage().persistent().get(&KEY_TIME));
        let vdf_policy = init
            .vdf_policy
            .clone()
            .or_else(|| env.storage().persistent().get(&KEY_VDF));

        let salt = next_salt(&env, &init.merchant);
        let wasm_hash: BytesN<32> = env
            .storage()
            .persistent()
            .get(&KEY_VAULT_WASM)
            .ok_or(Error::NotInitialized)?;

        let vault_init = VaultInit {
            merchant: init.merchant.clone(),
            token: init.token.clone(),
            time_policy,
            vdf_policy,
            fee_bps: init.fee_bps,
            fee_recipient: init.fee_recipient.clone(),
            refund_window: init.refund_window,
            deadline: init.deadline,
            vdf_delay: init.vdf_delay,
        };

        let vault = env
            .deployer()
            .with_current_contract(salt)
            .deploy_v2(wasm_hash, (vault_init,));

        let mut vaults: Vec<Address> = env
            .storage()
            .persistent()
            .get(&KEY_VAULTS)
            .unwrap_or_else(|| Vec::new(&env));
        vaults.push_back(vault.clone());
        env.storage().persistent().set(&KEY_VAULTS, &vaults);

        VaultDeployedEvent {
            merchant: init.merchant.clone(),
            vault: vault.clone(),
        }
        .publish(&env);

        Ok(vault)
    }

    /// Rotates the default time-policy address shares to future vaults.
    pub fn set_time_policy_contract(env: Env, address: Option<Address>) {
        require_admin(&env);
        match address {
            Some(policy) => env.storage().persistent().set(&KEY_TIME, &policy),
            None => env.storage().persistent().remove(&KEY_TIME),
        }
    }

    /// Rotates the default VDF-policy address shares to future vaults.
    pub fn set_vdf_policy_contract(env: Env, address: Option<Address>) {
        require_admin(&env);
        match address {
            Some(policy) => env.storage().persistent().set(&KEY_VDF, &policy),
            None => env.storage().persistent().remove(&KEY_VDF),
        }
    }

    /// Swaps the vault `wasm_hash` used for future deployments (admin only).
    pub fn set_vault_wasm(env: Env, vault_wasm_hash: BytesN<32>) -> Result<(), Error> {
        require_admin(&env);
        if vault_wasm_hash.to_array().iter().all(|b| *b == 0u8) {
            return Err(Error::InvalidWasmHash);
        }
        env.storage()
            .persistent()
            .set(&KEY_VAULT_WASM, &vault_wasm_hash);
        Ok(())
    }

    pub fn get_admin(env: Env) -> Option<Address> {
        env.storage().persistent().get(&KEY_ADMIN)
    }

    pub fn get_vault_wasm(env: Env) -> Option<BytesN<32>> {
        env.storage().persistent().get(&KEY_VAULT_WASM)
    }

    pub fn get_time_policy_contract(env: Env) -> Option<Address> {
        env.storage().persistent().get(&KEY_TIME)
    }

    pub fn get_vdf_policy_contract(env: Env) -> Option<Address> {
        env.storage().persistent().get(&KEY_VDF)
    }

    pub fn get_vaults(env: Env) -> Vec<Address> {
        env.storage()
            .persistent()
            .get::<_, Vec<Address>>(&KEY_VAULTS)
            .unwrap_or_else(|| Vec::new(&env))
    }

    pub fn get_next_salt(env: Env) -> u128 {
        env.storage()
            .persistent()
            .get::<_, u128>(&KEY_NEXT_SALT)
            .unwrap_or(0)
    }
}

fn init(
    env: &Env,
    admin: Address,
    vault_wasm_hash: BytesN<32>,
    time_policy: Option<Address>,
    vdf_policy: Option<Address>,
) -> Result<(), Error> {
    if env.storage().persistent().has(&KEY_INITED) {
        return Err(Error::AlreadyInitialized);
    }
    admin.require_auth();
    env.storage().persistent().set(&KEY_ADMIN, &admin);
    env.storage()
        .persistent()
        .set(&KEY_VAULT_WASM, &vault_wasm_hash);
    if let Some(policy) = &time_policy {
        env.storage().persistent().set(&KEY_TIME, policy);
    }
    if let Some(policy) = &vdf_policy {
        env.storage().persistent().set(&KEY_VDF, policy);
    }
    env.storage().persistent().set(&KEY_NEXT_SALT, &0u128);
    env.storage().persistent().set(&KEY_INITED, &true);
    Ok(())
}

fn require_initialized(env: &Env) -> Result<(), Error> {
    if env.storage().persistent().has(&KEY_INITED) {
        Ok(())
    } else {
        Err(Error::NotInitialized)
    }
}

fn require_admin(env: &Env) {
    let admin: Address = env
        .storage()
        .persistent()
        .get(&KEY_ADMIN)
        .expect("factory must be initialized");
    admin.require_auth();
}

fn next_salt(env: &Env, merchant: &Address) -> BytesN<32> {
    let counter: u128 = env.storage().persistent().get(&KEY_NEXT_SALT).unwrap_or(0);
    let mut buf = merchant.clone().to_xdr(env);
    buf.append(&counter.to_xdr(env));
    let salt: BytesN<32> = env.crypto().sha256(&buf).into();
    env.storage()
        .persistent()
        .set(&KEY_NEXT_SALT, &(counter + 1));
    salt
}
