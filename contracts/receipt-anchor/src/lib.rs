#![no_std]

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, Address, BytesN, Env, Vec,
};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    BatchNotFound = 4,
    BatchTooLarge = 5,
}

#[contracttype]
pub enum DataKey {
    Admin,
    BatchCount,
    Batch(u64),
    PrunedUpTo,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchRecord {
    pub root: BytesN<32>,
    pub count: u32,
    pub period_start: u64,
    pub period_end: u64,
    pub anchored_ledger: u32,
}

/// Emitted when a merchant anchors a batch of receipts.
///
/// Topics: `("anchor_event", batch_id)`. The data map mirrors [`BatchRecord`], so
/// indexers can decode it with the same shape returned by `get_batch`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnchorEvent {
    #[topic]
    pub batch_id: u64,
    pub root: BytesN<32>,
    pub count: u32,
    pub period_start: u64,
    pub period_end: u64,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PruneEvent {
    #[topic]
    pub start_batch_id: u64,
    pub end_batch_id: u64,
}

/// Approximately 30 days of ledgers, assuming ~5 seconds per ledger.
/// 60 * 60 * 24 * 30 / 5 = 518,400.
/// This ensures batches survive for long-term audit use before requiring a TTL bump or restoration.
const TTL_EXTEND: u32 = 518_400;
/// The threshold before TTL is actually bumped, to prevent spamming updates on every call.
const TTL_THRESHOLD: u32 = 100;

const MAX_BATCH_SIZE: u32 = 1000;

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
        env.storage().instance().set(&DataKey::PrunedUpTo, &1u64);
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    pub fn anchor_batch(
        env: Env,
        root: BytesN<32>,
        count: u32,
        period_start: u64,
        period_end: u64,
    ) -> Result<u64, Error> {
        if count > MAX_BATCH_SIZE {
            return Err(Error::BatchTooLarge);
        }

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
            anchored_ledger: env.ledger().sequence(),
        };

        env.storage()
            .persistent()
            .set(&DataKey::Batch(batch_id), &record);
        env.storage()
            .instance()
            .set(&DataKey::BatchCount, &batch_id);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Batch(batch_id), TTL_THRESHOLD, TTL_EXTEND);

        AnchorEvent {
            batch_id,
            root: record.root,
            count: record.count,
            period_start: record.period_start,
            period_end: record.period_end,
        }
        .publish(&env);

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

    pub fn get_batch_count(env: Env) -> Result<u64, Error> {
        env.storage()
            .instance()
            .get(&DataKey::BatchCount)
            .ok_or(Error::NotInitialized)
    }

    pub fn extend_batch_ttl(env: Env, batch_id: u64) -> Result<(), Error> {
        if !env.storage().persistent().has(&DataKey::Batch(batch_id)) {
            return Err(Error::BatchNotFound);
        }
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Batch(batch_id), TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    pub fn prune_batches(env: Env, before_ledger: u32) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        let start_batch_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::PrunedUpTo)
            .unwrap_or(1);
        let batch_count: u64 = env
            .storage()
            .instance()
            .get(&DataKey::BatchCount)
            .unwrap_or(0);

        let mut pruned_up_to = start_batch_id;

        while pruned_up_to <= batch_count {
            if let Some(record) = env
                .storage()
                .persistent()
                .get::<_, BatchRecord>(&DataKey::Batch(pruned_up_to))
            {
                if record.anchored_ledger < before_ledger {
                    env.storage()
                        .persistent()
                        .remove(&DataKey::Batch(pruned_up_to));
                    pruned_up_to += 1;
                } else {
                    break;
                }
            } else {
                // If it's not present, it might have been manually deleted or we skipped it.
                // We should just increment and continue.
                pruned_up_to += 1;
            }
        }

        if pruned_up_to > start_batch_id {
            env.storage()
                .instance()
                .set(&DataKey::PrunedUpTo, &pruned_up_to);
            PruneEvent {
                start_batch_id,
                end_batch_id: pruned_up_to,
            }
            .publish(&env);
        }

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }
}

mod fuzz_test;
mod test;
