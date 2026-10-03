//! Flash loans against the merchant's escrowed receivables (issue #442).
//!
//! The merchant (admin) may borrow any part of the vault's liquid float — the
//! escrowed refund reserve it has deposited — provided the loan is repaid,
//! plus a [`FLASH_LOAN_FEE_BPS`] premium, before the same invocation returns:
//!
//! 1. The vault records its token balance and transfers `amount` to the
//!    `receiver` contract.
//! 2. It calls [`FlashLoanReceiver::on_flash_loan`] on the receiver, which may
//!    use the funds however it likes but must transfer `amount + fee` back to
//!    the vault before returning.
//! 3. The vault checks its balance grew by at least `fee` over the starting
//!    balance and forwards the fee to the treasury (the fee recipient).
//!
//! Any shortfall reverts the whole invocation with
//! [`Error::InsufficientFloat`], rolling back the loan transfer, so escrowed
//! funds can never leave the vault outside a single atomic transaction.
//! Principal deployed to a yield strategy is not lendable: only the liquid
//! balance is. The receiver is untrusted and runs under the vault's
//! reentrancy lock, so it cannot call back into any guarded entry point
//! (`withdraw`, `refund`, `deposit`, …) while holding the loan.

use accensa_common::{storage::extend_instance_ttl, Error};
use soroban_sdk::{contractclient, contractevent, token, Address, Bytes, Env};

use crate::{active_fee_recipient, refund_fee, DataKey, TTL_EXTEND, TTL_THRESHOLD};

/// Flash loan premium in basis points: 9 bps = 0.09%, rounded up to the
/// token's smallest unit so no non-zero loan is ever fee-free.
pub const FLASH_LOAN_FEE_BPS: u32 = 9;

/// Interface a flash loan receiver contract must implement.
#[contractclient(name = "FlashLoanReceiverClient")]
pub trait FlashLoanReceiver {
    /// Called by the vault after `amount` of `token` has been transferred to
    /// the receiver. Before returning, the receiver must transfer
    /// `amount + fee` of `token` back to the vault. `initiator` is the
    /// merchant that authorized the loan and `data` is passed through
    /// unchanged from `flash_loan`.
    fn on_flash_loan(
        env: Env,
        initiator: Address,
        token: Address,
        amount: i128,
        fee: i128,
        data: Bytes,
    );
}

/// Emitted when a flash loan is repaid in full.
///
/// Topics: `("flash_loan_event", receiver: Address)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlashLoanEvent {
    #[topic]
    pub receiver: Address,
    pub amount: i128,
    /// Premium charged and forwarded to `treasury`.
    pub fee: i128,
    pub treasury: Address,
}

/// Premium owed on a flash loan of `amount`.
pub fn flash_loan_fee(amount: i128) -> i128 {
    refund_fee(amount, FLASH_LOAN_FEE_BPS)
}

pub(crate) fn flash_loan(
    env: &Env,
    receiver: Address,
    amount: i128,
    data: Bytes,
) -> Result<i128, Error> {
    accensa_common::reentrancy::ReentrancyGuard::acquire(env)?;

    if env
        .storage()
        .instance()
        .get(&DataKey::IsPaused)
        .unwrap_or(false)
    {
        return Err(Error::Paused);
    }
    if amount <= 0 {
        return Err(Error::InvalidAmount);
    }
    let vault = env.current_contract_address();
    if receiver == vault {
        return Err(Error::SelfTransfer);
    }

    let merchant: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(Error::NotInitialized)?;
    merchant.require_auth();

    let token_addr: Address = env
        .storage()
        .instance()
        .get(&DataKey::Token)
        .ok_or(Error::NotInitialized)?;
    let token_client = token::Client::new(env, &token_addr);

    let balance_before = token_client.balance(&vault);
    if amount > balance_before {
        return Err(Error::InsufficientFloat);
    }
    let fee = flash_loan_fee(amount);

    token_client.transfer(&vault, &receiver, &amount);
    FlashLoanReceiverClient::new(env, &receiver).on_flash_loan(
        &merchant,
        &token_addr,
        &amount,
        &fee,
        &data,
    );

    let required = balance_before.checked_add(fee).ok_or(Error::MathOverflow)?;
    if token_client.balance(&vault) < required {
        return Err(Error::InsufficientFloat);
    }

    let treasury = active_fee_recipient(env);
    if treasury != vault {
        token_client.transfer(&vault, &treasury, &fee);
    }

    FlashLoanEvent {
        receiver,
        amount,
        fee,
        treasury,
    }
    .publish(env);

    extend_instance_ttl(env, TTL_THRESHOLD, TTL_EXTEND);
    accensa_common::reentrancy::ReentrancyGuard::release(env);
    Ok(fee)
}
