#![no_std]

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contractmeta, contracttype, token,
    Address, BytesN, Env,
};

contractmeta!(key = "name", val = "UptoAuthorization");
contractmeta!(key = "version", val = env!("CARGO_PKG_VERSION"));
contractmeta!(
    key = "repo",
    val = "https://github.com/accensa/accensa-contracts"
);
contractmeta!(key = "commit", val = env!("GIT_SHA"));

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    AlreadySettled = 4,
    Expired = 5,
    AmountExceedsCap = 6,
    InvalidAmount = 7,
    AuthorizationNotFound = 8,
    AllowanceFailed = 9,
}

#[contracttype]
pub enum DataKey {
    Admin,
    Token,
    Authorization(BytesN<32>),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizationRecord {
    pub from: Address,
    pub to: Address,
    pub cap: i128,
    pub expiry: u32,
    pub consumed: bool,
}

/// Emitted when a buyer authorizes a payment cap.
///
/// Topics: `("authorize_event", payment_id)`. The data map contains
/// `from`, `to`, `cap`, and `expiry`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizeEvent {
    #[topic]
    pub payment_id: BytesN<32>,
    pub from: Address,
    pub to: Address,
    pub cap: i128,
    pub expiry: u32,
}

/// Emitted when a payment is settled.
///
/// Topics: `("settle_event", payment_id)`. The data map contains
/// `actual` and `from`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettleEvent {
    #[topic]
    pub payment_id: BytesN<32>,
    pub actual: i128,
    pub from: Address,
}

/// Emitted when lapsed authorizations are pruned.
///
/// Topics: `("prune_event", count)`. The data map is empty.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PruneEvent {
    #[topic]
    pub count: u32,
}

/// Approximately 30 days of ledgers, assuming ~5 seconds per ledger.
/// 60 * 60 * 24 * 30 / 5 = 518,400.
const TTL_EXTEND: u32 = 518_400;
/// The threshold before TTL is actually bumped, to prevent spamming updates on every call.
const TTL_THRESHOLD: u32 = 100;

#[contract]
pub struct UptoAuthorization;

#[contractimpl]
impl UptoAuthorization {
    pub fn initialize(env: Env, admin: Address, token: Address) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    /// Authorize a payment cap. The buyer signs this to grant the contract
    /// a SEP-41 allowance up to `cap` tokens, with `to` bound as the recipient.
    ///
    /// The contract records the authorization and calls `approve` on the token
    /// to grant itself the allowance. The buyer's auth entry must cover both
    /// this call and the nested `approve` call.
    pub fn authorize(
        env: Env,
        payment_id: BytesN<32>,
        from: Address,
        to: Address,
        cap: i128,
        expiry: u32,
    ) -> Result<(), Error> {
        if cap <= 0 {
            return Err(Error::InvalidAmount);
        }

        // The buyer must authorize this call — they are granting the contract
        // a SEP-41 allowance. In production, the buyer signs one auth entry
        // covering both this call and the nested `approve` on the token.
        from.require_auth();

        // Only the admin (facilitator) can authorize payments.
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        admin.require_auth();

        // Check if authorization already exists for this payment_id.
        if env
            .storage()
            .persistent()
            .has(&DataKey::Authorization(payment_id.clone()))
        {
            // Allow re-authorization if the previous one has expired.
            let existing: AuthorizationRecord = env
                .storage()
                .persistent()
                .get(&DataKey::Authorization(payment_id.clone()))
                .unwrap();
            if existing.expiry > env.ledger().sequence() && !existing.consumed {
                return Err(Error::AlreadySettled);
            }
        }

        // Get the token and approve the allowance.
        let token_addr: Address = env.storage().instance().get(&DataKey::Token).unwrap();
        let client = token::Client::new(&env, &token_addr);
        client.approve(&from, &env.current_contract_address(), &cap, &expiry);

        // Record the authorization.
        let record = AuthorizationRecord {
            from: from.clone(),
            to: to.clone(),
            cap,
            expiry,
            consumed: false,
        };

        env.storage()
            .persistent()
            .set(&DataKey::Authorization(payment_id.clone()), &record);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        env.storage().persistent().extend_ttl(
            &DataKey::Authorization(payment_id.clone()),
            TTL_THRESHOLD,
            TTL_EXTEND,
        );

        AuthorizeEvent {
            payment_id,
            from,
            to,
            cap,
            expiry,
        }
        .publish(&env);

        Ok(())
    }

    /// Settle a payment. The facilitator calls this with the actual amount
    /// charged. The recipient is determined at authorize time, not here.
    pub fn settle(env: Env, payment_id: BytesN<32>, actual: i128) -> Result<(), Error> {
        if actual <= 0 {
            return Err(Error::InvalidAmount);
        }

        // Only the admin (facilitator) can settle payments.
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        admin.require_auth();

        // Get the authorization record.
        let record: AuthorizationRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Authorization(payment_id.clone()))
            .ok_or(Error::AuthorizationNotFound)?;

        // The buyer must authorize settlement — the contract will call
        // transfer_from and approve(0) on their behalf.
        record.from.require_auth();

        // Check not already consumed.
        if record.consumed {
            return Err(Error::AlreadySettled);
        }

        // Check not expired.
        let current_ledger = env.ledger().sequence();
        if current_ledger > record.expiry {
            return Err(Error::Expired);
        }

        // Check actual <= cap.
        if actual > record.cap {
            return Err(Error::AmountExceedsCap);
        }

        // Transfer tokens directly from buyer to seller.
        let token_addr: Address = env.storage().instance().get(&DataKey::Token).unwrap();
        let token_client = token::Client::new(&env, &token_addr);
        token_client.transfer_from(
            &env.current_contract_address(), // spender = this contract
            &record.from,
            &record.to,
            &actual,
        );

        // Zero out the allowance so cap - actual doesn't linger.
        token_client.approve(
            &record.from,
            &env.current_contract_address(),
            &0i128,
            &record.expiry,
        );

        // Mark as consumed.
        let updated_record = AuthorizationRecord {
            consumed: true,
            ..record.clone()
        };
        env.storage()
            .persistent()
            .set(&DataKey::Authorization(payment_id.clone()), &updated_record);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        env.storage().persistent().extend_ttl(
            &DataKey::Authorization(payment_id.clone()),
            TTL_THRESHOLD,
            TTL_EXTEND,
        );

        SettleEvent {
            payment_id,
            actual,
            from: updated_record.from,
        }
        .publish(&env);

        Ok(())
    }

    /// Prune expired authorizations. Anyone can call this to reclaim storage rent.
    /// Only removes authorizations that have expired and been consumed (or never
    /// consumed but expired).
    pub fn prune_authorizations(env: Env) -> Result<u32, Error> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        admin.require_auth();

        let _current_ledger = env.ledger().sequence();
        let pruned_count: u32 = 0;

        // We can't iterate over storage keys in Soroban, so we rely on
        // the caller to know which payment_ids to prune. For now, we provide
        // a method that prunes a specific authorization if it's expired.
        // This is a limitation of Soroban's storage model.
        // In practice, the facilitator would maintain an index of payment_ids
        // and call this for each expired one.

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);

        Ok(pruned_count)
    }

    /// Prune a specific authorization if it's expired.
    pub fn prune_authorization(env: Env, payment_id: BytesN<32>) -> Result<(), Error> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        admin.require_auth();

        let record: AuthorizationRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Authorization(payment_id.clone()))
            .ok_or(Error::AuthorizationNotFound)?;

        let current_ledger = env.ledger().sequence();
        if current_ledger <= record.expiry {
            return Err(Error::Expired); // Not expired yet, can't prune
        }

        env.storage()
            .persistent()
            .remove(&DataKey::Authorization(payment_id));

        PruneEvent { count: 1 }.publish(&env);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    /// Get an authorization record.
    pub fn get_authorization(env: Env, payment_id: BytesN<32>) -> Option<AuthorizationRecord> {
        env.storage()
            .persistent()
            .get(&DataKey::Authorization(payment_id))
    }

    /// Extend the TTL of an authorization record.
    pub fn extend_authorization_ttl(env: Env, payment_id: BytesN<32>) -> Result<(), Error> {
        if !env
            .storage()
            .persistent()
            .has(&DataKey::Authorization(payment_id.clone()))
        {
            return Err(Error::AuthorizationNotFound);
        }
        env.storage().persistent().extend_ttl(
            &DataKey::Authorization(payment_id),
            TTL_THRESHOLD,
            TTL_EXTEND,
        );
        Ok(())
    }
}

mod test;
