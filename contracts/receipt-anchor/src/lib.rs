#![no_std]

use soroban_sdk::{contract, contracterror, contractimpl, contracttype, Address, BytesN, Env, Vec};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    BatchNotFound = 4,
}

#[contracttype]
pub enum DataKey {
    Admin,
    BatchCount,
    Batch(u64),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchRecord {
    pub root: BytesN<32>,
    pub count: u32,
    pub period_start: u64,
    pub period_end: u64,
}

#[contract]
pub struct ReceiptAnchor;

#[contractimpl]
impl ReceiptAnchor {
    pub fn initialize(env: Env, merchant: Address) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Admin, &merchant);
        env.storage().instance().set(&DataKey::BatchCount, &0u64);
        env.storage().instance().extend_ttl(100, 100000);
        Ok(())
    }

    pub fn anchor_batch(
        env: Env,
        root: BytesN<32>,
        count: u32,
        period_start: u64,
        period_end: u64,
    ) -> Result<u64, Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        let mut batch_id: u64 = env.storage().instance().get(&DataKey::BatchCount).unwrap();
        batch_id += 1;

        let record = BatchRecord {
            root: root.clone(),
            count,
            period_start,
            period_end,
        };

        env.storage()
            .persistent()
            .set(&DataKey::Batch(batch_id), &record);
        env.storage()
            .instance()
            .set(&DataKey::BatchCount, &batch_id);

        env.storage().instance().extend_ttl(100, 100000);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Batch(batch_id), 100, 100000);

        env.events()
            .publish((soroban_sdk::symbol_short!("anchored"), batch_id), record);

        Ok(batch_id)
    }

    pub fn get_batch(env: Env, batch_id: u64) -> Result<BatchRecord, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::Batch(batch_id))
            .ok_or(Error::BatchNotFound)
    }

    pub fn verify_receipt(
        env: Env,
        batch_id: u64,
        leaf: BytesN<32>,
        proof: Vec<BytesN<32>>,
    ) -> Result<bool, Error> {
        let batch = Self::get_batch(env.clone(), batch_id)?;
        let mut computed_hash = leaf.to_array();

        for sibling_bytes in proof.into_iter() {
            let sibling = sibling_bytes.to_array();
            let mut combined = [0u8; 64];
            if computed_hash <= sibling {
                combined[..32].copy_from_slice(&computed_hash);
                combined[32..].copy_from_slice(&sibling);
            } else {
                combined[..32].copy_from_slice(&sibling);
                combined[32..].copy_from_slice(&computed_hash);
            }
            computed_hash = env
                .crypto()
                .sha256(&soroban_sdk::Bytes::from_slice(&env, &combined))
                .to_array();
        }

        Ok(computed_hash == batch.root.to_array())
    }
}

mod test;
