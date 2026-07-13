#![no_std]

use soroban_sdk::{contract, contractimpl, contracttype, Address, BytesN, Env, Vec, Bytes};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchRecord {
    pub root: BytesN<32>,
    pub count: u32,
    pub period_start: u64,
    pub period_end: u64,
    pub anchored_at: u64,
}

#[contracttype]
pub enum DataKey {
    Admin,
    Merchant,
    BatchCount,
    Batch(u64),
}

#[contract]
pub struct ReceiptAnchor;

#[contractimpl]
impl ReceiptAnchor {
    pub fn initialize(env: Env, admin: Address, merchant: Address) {
        assert!(!env.storage().instance().has(&DataKey::Admin), "Already initialized");
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Merchant, &merchant);
        env.storage().instance().set(&DataKey::BatchCount, &0u64);
    }

    pub fn anchor_batch(env: Env, root: BytesN<32>, count: u32, period_start: u64, period_end: u64) -> u64 {
        let merchant: Address = env.storage().instance().get(&DataKey::Merchant).expect("Not initialized");
        merchant.require_auth();

        let mut batch_count: u64 = env.storage().instance().get(&DataKey::BatchCount).unwrap();
        batch_count += 1;

        let record = BatchRecord {
            root: root.clone(),
            count,
            period_start,
            period_end,
            anchored_at: env.ledger().timestamp(),
        };

        env.storage().persistent().set(&DataKey::Batch(batch_count), &record);
        env.storage().instance().set(&DataKey::BatchCount, &batch_count);

        env.events().publish(("anchor", batch_count), (root, count, period_start, period_end));

        batch_count
    }

    pub fn get_batch(env: Env, batch_id: u64) -> BatchRecord {
        env.storage().persistent().get(&DataKey::Batch(batch_id)).expect("Batch not found")
    }

    pub fn verify_receipt(env: Env, batch_id: u64, leaf: BytesN<32>, proof: Vec<BytesN<32>>) -> bool {
        let batch: BatchRecord = env.storage().persistent().get(&DataKey::Batch(batch_id)).expect("Batch not found");
        
        let mut curr_hash = leaf;
        for p in proof.into_iter() {
            let mut bytes = Bytes::new(&env);
            if curr_hash < p {
                bytes.extend_from_array(&curr_hash.to_array());
                bytes.extend_from_array(&p.to_array());
            } else {
                bytes.extend_from_array(&p.to_array());
                bytes.extend_from_array(&curr_hash.to_array());
            }
            curr_hash = env.crypto().sha256(&bytes);
        }

        curr_hash == batch.root
    }

    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) {
        let admin: Address = env.storage().instance().get(&DataKey::Admin).expect("Not initialized");
        admin.require_auth();
        env.deployer().update_current_contract_wasm(new_wasm_hash);
    }
}
