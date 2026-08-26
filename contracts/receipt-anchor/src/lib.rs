use soroban_sdk::{contract, contracterror, contractimpl, contracttype, Address, BytesN, Env, IntoVal, Symbol, Val, Vec};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ReceiptAnchorError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    BatchTooLarge = 4,
    BatchNotFound = 5,
    BatchIndexOverflow = 6,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchRecord {
    pub root: BytesN<32>,       // SHA-256 Merkle root of the receipt batch
    pub count: u32,             // Number of receipt leaves in the batch (<= MAX_BATCH_SIZE)
    pub period_start: u64,      // Unix timestamp (seconds) or logical start of the billing period
    pub period_end: u64,        // Unix timestamp (seconds) or logical end of the billing period
    pub anchored_ledger: u32,   // Ledger sequence number when the batch was anchored on-chain
}

#[contracttype]
#[derive(Clone, Debug)]
pub enum DataKey {
    Admin,
    BatchCount,
    PrunedUpTo,
    Batch(u64),
}

#[contracttype]
#[derive(Clone, Debug)]
pub enum AnchorEvent {
    Anchor(BytesN<32>), // indexed by ("anchor_event", batch_id)
}

#[contracttype]
#[derive(Clone, Debug)]
pub enum PruneEvent {
    Prune(u64), // indexed by ("prune_event", start_batch_id)
}

pub const MAX_BATCH_SIZE: u32 = 1000;

#[contract]
pub struct ReceiptAnchor;

#[contractimpl]
impl ReceiptAnchor {
    /// Initializes the receipt anchor contract by binding it to the given merchant admin address.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `merchant` - The address of the merchant admin who is authorized to anchor batches and prune old records.
    ///
    /// # Errors
    /// * `ReceiptAnchorError::AlreadyInitialized` - If the contract has already been initialized.
    ///
    /// # Authorization
    /// Requires no pre-existing auth for initialization, but sets the admin address.
    pub fn initialize(env: Env, merchant: Address) -> Result<(), ReceiptAnchorError> {
        if env.storage().persistent().has(&DataKey::Admin) {
            return Err(ReceiptAnchorError::AlreadyInitialized);
        }
        env.storage().persistent().set(&DataKey::Admin, &merchant);
        env.storage().persistent().set(&DataKey::BatchCount, &0u64);
        env.storage().persistent().set(&DataKey::PrunedUpTo, &1u64);
        Ok(())
    }

    /// Anchors a new Merkle batch of payment receipts on-chain.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `root` - The 32-byte SHA-256 Merkle root of the receipt batch.
    /// * `count` - The number of receipt leaves included in this batch. Must be $\le$ [`MAX_BATCH_SIZE`] (1000).
    /// * `period_start` - The start timestamp or logical sequence of the billing period.
    /// * `period_end` - The end timestamp or logical sequence of the billing period.
    ///
    /// # Returns
    /// Returns the assigned sequential `batch_id` (`u64`) for the newly anchored batch.
    ///
    /// # Errors
    /// * `ReceiptAnchorError::NotInitialized` - If the contract has not been initialized.
    /// * `ReceiptAnchorError::Unauthorized` - If the caller is not the merchant admin.
    /// * `ReceiptAnchorError::BatchTooLarge` - If `count` exceeds [`MAX_BATCH_SIZE`].
    /// * `ReceiptAnchorError::BatchIndexOverflow` - If the internal batch counter overflows a `u64`.
    ///
    /// # Authorization
    /// Requires authorization from the merchant admin (`Admin`).
    pub fn anchor_batch(
        env: Env,
        root: BytesN<32>,
        count: u32,
        period_start: u64,
        period_end: u64,
    ) -> Result<u64, ReceiptAnchorError> {
        let admin: Address = env
            .storage()
            .persistent()
            .get(&DataKey::Admin)
            .ok_or(ReceiptAnchorError::NotInitialized)?;
        admin.require_auth();

        if count > MAX_BATCH_SIZE {
            return Err(ReceiptAnchorError::BatchTooLarge);
        }

        let mut batch_count: u64 = env
            .storage()
            .persistent()
            .get(&DataKey::BatchCount)
            .unwrap_or(0);

        batch_count = batch_count
            .checked_add(1)
            .ok_or(ReceiptAnchorError::BatchIndexOverflow)?;

        let record = BatchRecord {
            root,
            count,
            period_start,
            period_end,
            anchored_ledger: env.ledger().sequence(),
        };

        env.storage().persistent().set(&DataKey::Batch(batch_count), &record);
        env.storage().persistent().set(&DataKey::BatchCount, &batch_count);

        // Emit event: AnchorEvent(batch_id) with data matching BatchRecord structure
        let topics = (Symbol::new(&env, "anchor_event"), batch_count);
        env.events().publish(
            topics,
            (
                record.root,
                record.count,
                record.period_start,
                record.period_end,
                record.anchored_ledger,
            ),
        );

        Ok(batch_count)
    }

    /// Reads the stored record for a previously anchored batch.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `batch_id` - The sequential identifier of the batch to retrieve.
    ///
    /// # Returns
    /// Returns `BatchRecord` containing the Merkle root, leaf count, billing period bounds, and anchoring ledger.
    ///
    /// # Errors
    /// * `ReceiptAnchorError::NotInitialized` - If the contract has not been initialized.
    /// * `ReceiptAnchorError::BatchNotFound` - If the batch ID does not exist or has been pruned.
    ///
    /// # Authorization
    /// Read-only; requires no authorization.
    pub fn get_batch(env: Env, batch_id: u64) -> Result<BatchRecord, ReceiptAnchorError> {
        if !env.storage().persistent().has(&DataKey::Admin) {
            return Err(ReceiptAnchorError::NotInitialized);
        }
        env.storage()
            .persistent()
            .get(&DataKey::Batch(batch_id))
            .ok_or(ReceiptAnchorError::BatchNotFound)
    }

    /// Returns the total number of batches anchored so far.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    ///
    /// # Returns
    /// Returns the total batch count as a `u64`.
    ///
    /// # Errors
    /// * `ReceiptAnchorError::NotInitialized` - If the contract has not been initialized.
    ///
    /// # Authorization
    /// Read-only; requires no authorization.
    pub fn get_batch_count(env: Env) -> Result<u64, ReceiptAnchorError> {
        let count: u64 = env
            .storage()
            .persistent()
            .get(&DataKey::BatchCount)
            .ok_or(ReceiptAnchorError::NotInitialized)?;
        Ok(count)
    }

    /// Returns the maximum allowed number of receipts in a single batch.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    ///
    /// # Returns
    /// Returns [`MAX_BATCH_SIZE`] (`u32`, currently 1000).
    ///
    /// # Authorization
    /// Read-only; requires no authorization.
    pub fn get_max_batch_size(_env: Env) -> u32 {
        MAX_BATCH_SIZE
    }

    /// Verifies whether a specific receipt leaf hash is part of an anchored batch using a Merkle proof.
    ///
    /// Uses sorted-pair SHA-256 hashing where sibling hashes are concatenated smaller-hash-first.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `batch_id` - The identifier of the anchored batch against which to verify.
    /// * `leaf` - The 32-byte receipt leaf hash.
    /// * `proof` - A vector of 32-byte sibling hashes representing the Merkle authentication path.
    ///
    /// # Returns
    /// Returns `true` if the proof successfully validates the leaf against the batch's Merkle root, and `false` otherwise.
    ///
    /// # Errors
    /// * `ReceiptAnchorError::BatchNotFound` - If the batch does not exist or has been pruned.
    ///
    /// # Authorization
    /// Read-only, free to call; requires no authorization.
    pub fn verify_receipt(
        env: Env,
        batch_id: u64,
        leaf: BytesN<32>,
        proof: Vec<BytesN<32>>,
    ) -> Result<bool, ReceiptAnchorError> {
        let record = Self::get_batch(env.clone(), batch_id)?;
        let mut current = leaf;

        for sibling in proof.iter() {
            current = if current < sibling {
                let mut buf = [0u8; 64];
                buf[..32].copy_from_slice(&current.to_array());
                buf[32..].copy_from_slice(&sibling.to_array());
                env.crypto().sha256(&BytesN::from_array(&env, &buf))
            } else {
                let mut buf = [0u8; 64];
                buf[..32].copy_from_slice(&sibling.to_array());
                buf[32..].copy_from_slice(&current.to_array());
                env.crypto().sha256(&BytesN::from_array(&env, &buf))
            };
        }

        Ok(current == record.root)
    }

    /// Extends the TTL (Time-To-Live) of an anchored batch record to prevent state archival.
    ///
    /// This function is intentionally publicly callable by anyone, allowing agents or integrators
    /// to sponsor or maintain storage liveness for important receipt batches.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `batch_id` - The identifier of the batch record whose TTL should be extended.
    ///
    /// # Errors
    /// * `ReceiptAnchorError::BatchNotFound` - If the batch record does not exist or has been pruned.
    ///
    /// # Authorization
    /// Publicly callable; requires no authorization.
    pub fn extend_batch_ttl(env: Env, batch_id: u64) -> Result<(), ReceiptAnchorError> {
        if !env.storage().persistent().has(&DataKey::Batch(batch_id)) {
            return Err(ReceiptAnchorError::BatchNotFound);
        }
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Batch(batch_id), 500000, 500000);
        Ok(())
    }

    /// Deletes anchored batches older than a given ledger sequence number to reclaim storage rent.
    ///
    /// Pruning walks forward from an internal `PrunedUpTo` cursor and stops at the first batch
    /// that is not old enough, ensuring the deleted range always stays a contiguous prefix.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `before_ledger` - The ledger sequence threshold; batches anchored strictly before this ledger may be pruned.
    ///
    /// # Errors
    /// * `ReceiptAnchorError::NotInitialized` - If the contract has not been initialized.
    /// * `ReceiptAnchorError::Unauthorized` - If the caller is not the merchant admin.
    ///
    /// # Authorization
    /// Requires authorization from the merchant admin (`Admin`).
    pub fn prune_batches(env: Env, before_ledger: u32) -> Result<(), ReceiptAnchorError> {
        let admin: Address = env
            .storage()
            .persistent()
            .get(&DataKey::Admin)
            .ok_or(ReceiptAnchorError::NotInitialized)?;
        admin.require_auth();

        let mut pruned_up_to: u64 = env
            .storage()
            .persistent()
            .get(&DataKey::PrunedUpTo)
            .unwrap_or(1);

        let batch_count: u64 = env
            .storage()
            .persistent()
            .get(&DataKey::BatchCount)
            .unwrap_or(0);

        let start_pruned = pruned_up_to;

        while pruned_up_to <= batch_count {
            if let Some(record) = env
                .storage()
                .persistent()
                .get::<DataKey, BatchRecord>(&DataKey::Batch(pruned_up_to))
            {
                if record.anchored_ledger < before_ledger {
                    env.storage().persistent().remove(&DataKey::Batch(pruned_up_to));
                    pruned_up_to += 1;
                } else {
                    break;
                }
            } else {
                pruned_up_to += 1;
            }
        }

        if pruned_up_to > start_pruned {
            env.storage().persistent().set(&DataKey::PrunedUpTo, &pruned_up_to);
            let topics = (Symbol::new(&env, "prune_event"), start_pruned);
            env.events().publish(topics, pruned_up_to - 1);
        }

        Ok(())
    }
}
