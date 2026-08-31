//! # Spike: UptoAuthorization with Nested SEP-41 Approve
//!
//! **This is experimental research code for ADR-002 §6.2.**
//! It is NOT production code. It does NOT implement the final upto scheme.
//!
//! Purpose: Determine whether a single Soroban authorization entry can cover
//! a parent contract invocation that makes a nested sub-invocation to a
//! SEP-41 token's `approve` function.

use soroban_sdk::{contract, contracterror, contractimpl, contracttype, token, Address, Env};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SpikeError {
    AlreadyConsumed = 1,
    Expired = 2,
    AmountExceedsCap = 3,
    NotSettled = 4,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthRecord {
    pub from: Address,
    pub to: Address,
    pub cap: i128,
    pub expiry: u32,
    pub consumed: bool,
}

#[contract]
pub struct UptoAuthorization;

#[contractimpl]
impl UptoAuthorization {
    /// Bind a recipient and approve a token allowance for this contract.
    ///
    /// The nested `token.approve(from, spender=self, amount, expiry)` is the
    /// sub-invocation whose authorization coverage is under investigation.
    ///
    /// In the ADR-002 §4 construction, the payer signs a single auth tree
    /// covering both this call and the nested approve.
    pub fn authorize(
        env: Env,
        payment_id: u32,
        from: Address,
        to: Address,
        cap: i128,
        expiry: u32,
    ) -> Result<(), SpikeError> {
        if cap <= 0 {
            return Err(SpikeError::AmountExceedsCap);
        }

        // REQUEST AUTH: `from` must authorize this call so that the subsequent
        // nested `token.approve(from, ...)` is covered by `from`'s auth entry.
        // Without this, Soroban refuses the nested approve on behalf of `from`.
        from.require_auth();

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&"token")
            .ok_or(SpikeError::NotSettled)?;
        let token_client = token::Client::new(&env, &token_addr);

        // NESTED SUB-INVOCATION: approve this contract as spender on behalf of `from`.
        // This is the critical call — does the payer's auth entry cover it?
        token_client.approve(&from, &env.current_contract_address(), &cap, &expiry);

        let record = AuthRecord {
            from,
            to,
            cap,
            expiry,
            consumed: false,
        };
        env.storage().persistent().set(&payment_id, &record);

        Ok(())
    }

    /// Settle an authorization by transferring the actual amount.
    /// Clears the approval after settlement.
    pub fn settle(env: Env, payment_id: u32, actual: i128) -> Result<(), SpikeError> {
        let mut record: AuthRecord = env
            .storage()
            .persistent()
            .get(&payment_id)
            .ok_or(SpikeError::NotSettled)?;

        if record.consumed {
            return Err(SpikeError::AlreadyConsumed);
        }
        if actual > record.cap || actual <= 0 {
            return Err(SpikeError::AmountExceedsCap);
        }

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&"token")
            .ok_or(SpikeError::NotSettled)?;
        let token_client = token::Client::new(&env, &token_addr);

        // Transfer actual amount from the buyer to the seller.
        // The spender (this contract) uses the allowance.
        token_client.transfer_from(
            &env.current_contract_address(), // spender (authorized via approve)
            &record.from,                    // from
            &record.to,                      // to
            &actual,                         // amount
        );

        // Clear the approval.
        token_client.approve(&record.from, &env.current_contract_address(), &0, &0);

        record.consumed = true;
        env.storage().persistent().set(&payment_id, &record);

        Ok(())
    }

    /// Initialize with the token address.
    pub fn initialize(env: Env, token: Address) {
        env.storage().instance().set(&"token", &token);
    }
}

// ── Test module ──────────────────────────────────────────────────────────────
#[cfg(test)]
mod test;
