#![no_std]

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contractmeta, contracttype, token,
    Address, Bytes, BytesN, Env,
};

mod domain;
mod types;

pub use types::{max_settleable, AuthorizationRecord, BPS_DENOMINATOR, MAX_SLIPPAGE_BPS};

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
    /// `authorize_signed` was called for a buyer with no registered
    /// Ed25519 signer key (issue #416).
    SignerNotRegistered = 10,
    /// `max_slippage_bps` is above [`MAX_SLIPPAGE_BPS`].
    InvalidSlippage = 11,
    /// `cap` plus its slippage tolerance does not fit in an `i128`.
    AmountOverflow = 12,
}

#[contracttype]
pub enum DataKey {
    Admin,
    Token,
    Authorization(BytesN<32>),
    /// Persistent: the Ed25519 public key authorized to sign
    /// `authorize_signed` digests for this buyer address (issue #416).
    Signer(Address),
}

/// Emitted when a buyer authorizes a payment cap.
///
/// Topics: `("authorize_event", payment_id)`. The data map contains
/// `from`, `to`, `cap`, `expiry`, and `max_slippage_bps`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizeEvent {
    #[topic]
    pub payment_id: BytesN<32>,
    pub from: Address,
    pub to: Address,
    pub cap: i128,
    pub expiry: u32,
    /// Slippage tolerated above `cap`, in basis points (`0` = none).
    pub max_slippage_bps: u32,
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
    ///
    /// Settlement is capped at exactly `cap`; see
    /// [`Self::authorize_with_slippage`] to tolerate price movement.
    pub fn authorize(
        env: Env,
        payment_id: BytesN<32>,
        from: Address,
        to: Address,
        cap: i128,
        expiry: u32,
    ) -> Result<(), Error> {
        Self::authorize_with_slippage(env, payment_id, from, to, cap, expiry, 0)
    }

    /// Like [`Self::authorize`], but `settle` may charge up to
    /// `cap + floor(cap * max_slippage_bps / 10_000)` to absorb price movement
    /// in cross-currency settlements. The buyer's signature covers
    /// `max_slippage_bps`, and the token allowance is granted for that full
    /// maximum so `transfer_from` can cover it.
    ///
    /// Fails with [`Error::InvalidSlippage`] if `max_slippage_bps` exceeds
    /// [`MAX_SLIPPAGE_BPS`], or [`Error::AmountOverflow`] if the maximum does
    /// not fit in an `i128`.
    pub fn authorize_with_slippage(
        env: Env,
        payment_id: BytesN<32>,
        from: Address,
        to: Address,
        cap: i128,
        expiry: u32,
        max_slippage_bps: u32,
    ) -> Result<(), Error> {
        if cap <= 0 {
            return Err(Error::InvalidAmount);
        }
        if max_slippage_bps > MAX_SLIPPAGE_BPS {
            return Err(Error::InvalidSlippage);
        }
        let max_amount = max_settleable(cap, max_slippage_bps).ok_or(Error::AmountOverflow)?;

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
        client.approve(&from, &env.current_contract_address(), &max_amount, &expiry);

        // Record the authorization.
        let record = AuthorizationRecord {
            from: from.clone(),
            to: to.clone(),
            cap,
            expiry,
            consumed: false,
            max_slippage_bps,
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
            max_slippage_bps,
        }
        .publish(&env);

        Ok(())
    }

    /// Register the Ed25519 public key whose signatures
    /// [`authorize_signed`](Self::authorize_signed) will accept for `buyer`
    /// (issue #416).
    ///
    /// `buyer.require_auth()`, so only the buyer can bind a key to their
    /// address (or a contract acting as the buyer under its own auth rules).
    /// Binding via storage — instead of deriving the key from the address —
    /// keeps this usable for contract-address buyers, which cannot produce
    /// Ed25519 signatures themselves.
    pub fn register_signer(
        env: Env,
        buyer: Address,
        signer_pubkey: BytesN<32>,
    ) -> Result<(), Error> {
        buyer.require_auth();
        let key = DataKey::Signer(buyer);
        env.storage().persistent().set(&key, &signer_pubkey);
        // Threshold == extend_to so a freshly-written entry (already at the
        // network minimum TTL) is actually extended, not silently skipped.
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_EXTEND, TTL_EXTEND);
        Ok(())
    }

    /// Read-only: the registered signer key for `buyer`, if any (issue #416).
    pub fn get_signer(env: Env, buyer: Address) -> Option<BytesN<32>> {
        env.storage().persistent().get(&DataKey::Signer(buyer))
    }

    /// Read-only: this deployment's domain separator (issue #416) —
    /// `sha256(network_id ‖ contract_address ‖ protocol_version)`. Fetch
    /// this (or the digest below) when constructing an off-chain signature;
    /// never hardcode it.
    pub fn get_domain_separator(env: Env) -> BytesN<32> {
        domain::compute_domain_separator(&env)
    }

    /// Read-only: the exact digest `authorize_signed` will verify against
    /// (issue #416). Sign these 32 bytes with the registered Ed25519 key;
    /// the returned value already includes the domain separator, so a
    /// signature produced for it is bound to this network and this contract
    /// address.
    #[allow(clippy::too_many_arguments)]
    pub fn get_authorization_digest(
        env: Env,
        payment_id: BytesN<32>,
        from: Address,
        to: Address,
        cap: i128,
        expiry: u32,
    ) -> BytesN<32> {
        domain::authorization_digest(
            &env,
            &domain::compute_domain_separator(&env),
            &payment_id,
            &from,
            &to,
            cap,
            expiry,
        )
    }

    /// [`authorize`](Self::authorize) gated by an Ed25519 signature over the
    /// domain-separated digest (issue #416).
    ///
    /// The signature covers `network_id`, this contract's address, the
    /// protocol version, and the full authorization tuple, so a signature
    /// made on another network (or for another payment/cap) never verifies:
    /// `ed25519_verify` traps and the whole invocation rolls back before any
    /// allowance is granted. After verification the call proceeds through
    /// the regular [`authorize`](Self::authorize) path — `from.require_auth`
    /// still runs (the nested token `approve` needs it) — so the signature
    /// is an *additional* binding, not a replacement for host auth.
    #[allow(clippy::too_many_arguments)]
    pub fn authorize_signed(
        env: Env,
        payment_id: BytesN<32>,
        from: Address,
        to: Address,
        cap: i128,
        expiry: u32,
        signature: BytesN<64>,
    ) -> Result<(), Error> {
        let signer: BytesN<32> = env
            .storage()
            .persistent()
            .get(&DataKey::Signer(from.clone()))
            .ok_or(Error::SignerNotRegistered)?;
        env.storage().persistent().extend_ttl(
            &DataKey::Signer(from.clone()),
            TTL_EXTEND,
            TTL_EXTEND,
        );

        let digest = domain::authorization_digest(
            &env,
            &domain::compute_domain_separator(&env),
            &payment_id,
            &from,
            &to,
            cap,
            expiry,
        );
        let msg = Bytes::from(digest);
        // Traps on mismatch: a signature forged for a different network,
        // contract address, payment, recipient, cap, or expiry is rejected
        // here, before `authorize` touches any storage or allowance.
        env.crypto().ed25519_verify(&signer, &msg, &signature);

        Self::authorize(env, payment_id, from, to, cap, expiry)
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

        // Check actual <= cap + slippage tolerance.
        let max_amount = record.max_settleable().ok_or(Error::AmountOverflow)?;
        if actual > max_amount {
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

        // Zero out the allowance so max_amount - actual doesn't linger.
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

mod fuzz_test;
mod test;
