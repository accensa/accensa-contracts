//! Flash loan tests (issue #442).

use super::*;
use crate::flash_loan::{flash_loan_fee, FLASH_LOAN_FEE_BPS};
use crate::test_helpers::vault_init;
use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    Address, Bytes, Env,
};

const FLOAT: i128 = 1_000_000;
/// Spare balance the receiver holds to pay the premium.
const RECEIVER_FUNDS: i128 = 10_000;

// ── Mock receiver ──────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Copy)]
pub enum Mode {
    /// Repay `amount + fee`.
    Repay,
    /// Repay only the principal, skipping the premium.
    RepayPrincipal,
    /// Keep the borrowed funds.
    Default,
    /// Try to withdraw the float from the vault mid-loan.
    Reenter,
}

#[contracttype]
enum ReceiverKey {
    Vault,
    Mode,
}

#[contract]
pub struct MockReceiver;

#[contractimpl]
impl MockReceiver {
    pub fn setup(env: Env, vault: Address, mode: Mode) {
        env.storage().instance().set(&ReceiverKey::Vault, &vault);
        env.storage().instance().set(&ReceiverKey::Mode, &mode);
    }

    pub fn on_flash_loan(
        env: Env,
        _initiator: Address,
        token: Address,
        amount: i128,
        fee: i128,
        _data: Bytes,
    ) {
        let vault: Address = env.storage().instance().get(&ReceiverKey::Vault).unwrap();
        let mode: Mode = env.storage().instance().get(&ReceiverKey::Mode).unwrap();
        let token = TokenClient::new(&env, &token);
        let me = env.current_contract_address();
        match mode {
            Mode::Repay => token.transfer(&me, &vault, &(amount + fee)),
            Mode::RepayPrincipal => token.transfer(&me, &vault, &amount),
            Mode::Default => {}
            Mode::Reenter => {
                RefundVaultClient::new(&env, &vault).withdraw(&FLOAT, &me);
            }
        }
    }
}

// ── Setup ──────────────────────────────────────────────────────────────────

struct Setup {
    env: Env,
    client: RefundVaultClient<'static>,
    token: TokenClient<'static>,
    vault: Address,
    merchant: Address,
    receiver: Address,
}

fn setup(mode: Mode) -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let vault = env.register(RefundVault, (vault_init(&env, &merchant, &token, 100),));
    let client = RefundVaultClient::new(&env, &vault);
    client.deposit(&merchant, &FLOAT, &None);

    let receiver = env.register(MockReceiver, ());
    MockReceiverClient::new(&env, &receiver).setup(&vault, &mode);
    StellarAssetClient::new(&env, &token).mint(&receiver, &RECEIVER_FUNDS);

    Setup {
        token: TokenClient::new(&env, &token),
        env,
        client,
        vault,
        merchant,
        receiver,
    }
}

fn data(env: &Env) -> Bytes {
    Bytes::from_slice(env, b"arb")
}

// ── Fee ────────────────────────────────────────────────────────────────────

#[test]
fn fee_is_nine_bps_rounded_up() {
    assert_eq!(FLASH_LOAN_FEE_BPS, 9);
    assert_eq!(flash_loan_fee(1_000_000), 900);
    assert_eq!(flash_loan_fee(10_000), 9);
    // Sub-unit premiums round up, so no loan is free.
    assert_eq!(flash_loan_fee(1), 1);
    assert_eq!(flash_loan_fee(10_001), 10);
}

// ── Successful loans ───────────────────────────────────────────────────────

#[test]
fn repaid_loan_charges_fee_to_treasury() {
    let s = setup(Mode::Repay);
    let treasury = Address::generate(&s.env);
    s.client.set_fee_recipient(&treasury);

    let amount = 500_000;
    let fee = s.client.flash_loan(&s.receiver, &amount, &data(&s.env));

    assert_eq!(fee, 450);
    assert_eq!(s.token.balance(&s.vault), FLOAT);
    assert_eq!(s.token.balance(&treasury), fee);
    assert_eq!(s.token.balance(&s.receiver), RECEIVER_FUNDS - fee);
}

#[test]
fn entire_float_can_be_borrowed() {
    let s = setup(Mode::Repay);
    let fee = s.client.flash_loan(&s.receiver, &FLOAT, &data(&s.env));

    assert_eq!(fee, flash_loan_fee(FLOAT));
    assert_eq!(s.token.balance(&s.vault), FLOAT);
    // No fee recipient configured: the premium falls back to the merchant.
    assert_eq!(s.token.balance(&s.merchant), fee);
}

#[test]
fn lock_is_released_after_loan() {
    let s = setup(Mode::Repay);
    s.client.flash_loan(&s.receiver, &1_000, &data(&s.env));
    // A guarded entry point still works afterwards.
    s.client.withdraw(&1_000, &s.merchant);
    s.client.flash_loan(&s.receiver, &1_000, &data(&s.env));
}

// ── Forced reverts ─────────────────────────────────────────────────────────

#[test]
fn non_repayment_reverts() {
    let s = setup(Mode::Default);
    assert_eq!(
        s.client
            .try_flash_loan(&s.receiver, &500_000, &data(&s.env)),
        Err(Ok(Error::InsufficientFloat))
    );
    // The loan transfer was rolled back.
    assert_eq!(s.token.balance(&s.vault), FLOAT);
    assert_eq!(s.token.balance(&s.receiver), RECEIVER_FUNDS);
}

#[test]
fn principal_without_fee_reverts() {
    let s = setup(Mode::RepayPrincipal);
    assert_eq!(
        s.client
            .try_flash_loan(&s.receiver, &500_000, &data(&s.env)),
        Err(Ok(Error::InsufficientFloat))
    );
    assert_eq!(s.token.balance(&s.vault), FLOAT);
    assert_eq!(s.token.balance(&s.receiver), RECEIVER_FUNDS);
}

#[test]
fn reentry_during_loan_reverts() {
    let s = setup(Mode::Reenter);
    assert!(s
        .client
        .try_flash_loan(&s.receiver, &500_000, &data(&s.env))
        .is_err());
    assert_eq!(s.token.balance(&s.vault), FLOAT);
    assert_eq!(s.token.balance(&s.receiver), RECEIVER_FUNDS);
}

// ── Input validation ───────────────────────────────────────────────────────

#[test]
fn rejects_amount_above_float() {
    let s = setup(Mode::Repay);
    assert_eq!(
        s.client
            .try_flash_loan(&s.receiver, &(FLOAT + 1), &data(&s.env)),
        Err(Ok(Error::InsufficientFloat))
    );
}

#[test]
fn rejects_non_positive_amount() {
    let s = setup(Mode::Repay);
    assert_eq!(
        s.client.try_flash_loan(&s.receiver, &0, &data(&s.env)),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn rejects_vault_as_receiver() {
    let s = setup(Mode::Repay);
    assert_eq!(
        s.client.try_flash_loan(&s.vault, &1_000, &data(&s.env)),
        Err(Ok(Error::SelfTransfer))
    );
}

#[test]
fn rejects_while_paused() {
    let s = setup(Mode::Repay);
    s.client.pause();
    assert_eq!(
        s.client.try_flash_loan(&s.receiver, &1_000, &data(&s.env)),
        Err(Ok(Error::Paused))
    );
}

#[test]
fn requires_merchant_auth() {
    let s = setup(Mode::Repay);
    s.env.set_auths(&[]);
    assert!(s
        .client
        .try_flash_loan(&s.receiver, &1_000, &data(&s.env))
        .is_err());
    assert_eq!(s.token.balance(&s.vault), FLOAT);
}
