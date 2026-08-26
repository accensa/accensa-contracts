use soroban_sdk::{contract, contracterror, contractimpl, contracttype, token, Address, BytesN, Env, IntoVal, Symbol, Val, Vec};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum RefundVaultError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    Paused = 4,
    InvalidAmount = 5,
    OutsideWindow = 6,
    AlreadyRefunded = 7,
    RefundNotFound = 8,
    NoPendingAdmin = 9,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefundRecord {
    pub amount: i128,   // Refunded token amount (in token atomic units)
    pub recipient: Address, // Recipient address receiving the refunded funds
    pub ledger: u32,    // Ledger sequence number when the refund was executed
}

#[contracttype]
#[derive(Clone, Debug)]
pub enum DataKey {
    Admin,
    PendingAdmin,
    Token,
    RefundWindow,
    Paused,
    Refund(BytesN<32>),
}

#[contract]
pub struct RefundVault;

#[contractimpl]
impl RefundVault {
    /// Initializes the refund vault contract, setting the merchant admin, settlement token, and refund time window.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `merchant` - The address of the merchant admin controlling the vault.
    /// * `token` - The address of the Stellar Asset Contract (e.g., USDC) used for merchant float and refunds.
    /// * `refund_window_ledgers` - The validity window measured in ledgers against `paid_at_ledger`. A value of `0` disables expiry entirely.
    ///
    /// # Errors
    /// * `RefundVaultError::AlreadyInitialized` - If the contract has already been initialized.
    ///
    /// # Authorization
    /// Requires no pre-existing auth for initialization, but sets the admin address.
    pub fn initialize(
        env: Env,
        merchant: Address,
        token: Address,
        refund_window_ledgers: u32,
    ) -> Result<(), RefundVaultError> {
        if env.storage().persistent().has(&DataKey::Admin) {
            return Err(RefundVaultError::AlreadyInitialized);
        }
        env.storage().persistent().set(&DataKey::Admin, &merchant);
        env.storage().persistent().set(&DataKey::Token, &token);
        env.storage().persistent().set(&DataKey::RefundWindow, &refund_window_ledgers);
        env.storage().persistent().set(&DataKey::Paused, &false);
        Ok(())
    }

    /// Deposits settlement tokens into the vault to top up the merchant's float.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `from` - The address providing the tokens (must authorize the transfer).
    /// * `amount` - The token amount to deposit, in atomic units (must be $> 0$).
    ///
    /// # Errors
    /// * `RefundVaultError::NotInitialized` - If the contract has not been initialized.
    /// * `RefundVaultError::Paused` - If the vault is currently paused.
    /// * `RefundVaultError::InvalidAmount` - If `amount` is $\le 0$.
    ///
    /// # Authorization
    /// Requires authorization from the `from` address.
    pub fn deposit(env: Env, from: Address, amount: i128) -> Result<(), RefundVaultError> {
        let paused: bool = env.storage().persistent().get(&DataKey::Paused).unwrap_or(false);
        if paused {
            return Err(RefundVaultError::Paused);
        }
        if amount <= 0 {
            return Err(RefundVaultError::InvalidAmount);
        }
        let token_addr: Address = env
            .storage()
            .persistent()
            .get(&DataKey::Token)
            .ok_or(RefundVaultError::NotInitialized)?;

        from.require_auth();

        let client = token::Client::new(&env, &token_addr);
        client.transfer(&from, &env.current_contract_address(), &amount);

        let topics = (Symbol::new(&env, "deposit_event"), from.clone());
        env.events().publish(topics, amount);

        Ok(())
    }

    /// Refunds a payment to a recipient, subject to merchant policy checks (window and balance/pause).
    ///
    /// The `payment_ref` must correspond to the exact 32-byte receipt leaf hash anchored in `ReceiptAnchor`.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `payment_ref` - The 32-byte payment reference hash (matching the receipt leaf hash).
    /// * `recipient` - The address to receive the refunded tokens.
    /// * `amount` - The token amount to refund, in atomic units (must be $> 0$).
    /// * `paid_at_ledger` - The ledger sequence number when the original payment occurred, used against `refund_window_ledgers`.
    ///
    /// # Errors
    /// * `RefundVaultError::NotInitialized` - If the contract has not been initialized.
    /// * `RefundVaultError::Paused` - If the vault is currently paused.
    /// * `RefundVaultError::InvalidAmount` - If `amount` is $\le 0$.
    /// * `RefundVaultError::AlreadyRefunded` - If a refund for this `payment_ref` has already been executed (double-refund protection).
    /// * `RefundVaultError::OutsideWindow` - If the current ledger exceeds `paid_at_ledger + refund_window_ledgers` (unless window is 0).
    /// * [`token::Error`] - If the token transfer fails (e.g. insufficient vault float balance).
    ///
    /// # Authorization
    /// Requires authorization from the merchant admin (`Admin`).
    pub fn refund(
        env: Env,
        payment_ref: BytesN<32>,
        recipient: Address,
        amount: i128,
        paid_at_ledger: u32,
    ) -> Result<(), RefundVaultError> {
        let paused: bool = env.storage().persistent().get(&DataKey::Paused).unwrap_or(false);
        if paused {
            return Err(RefundVaultError::Paused);
        }
        if amount <= 0 {
            return Err(RefundVaultError::InvalidAmount);
        }

        let admin: Address = env
            .storage()
            .persistent()
            .get(&DataKey::Admin)
            .ok_or(RefundVaultError::NotInitialized)?;
        admin.require_auth();

        if env.storage().persistent().has(&DataKey::Refund(payment_ref.clone())) {
            return Err(RefundVaultError::AlreadyRefunded);
        }

        let window: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::RefundWindow)
            .unwrap_or(0);

        if window > 0 {
            let current_ledger = env.ledger().sequence();
            let deadline = paid_at_ledger.saturating_add(window);
            if current_ledger > deadline {
                return Err(RefundVaultError::OutsideWindow);
            }
        }

        let token_addr: Address = env
            .storage()
            .persistent()
            .get(&DataKey::Token)
            .ok_or(RefundVaultError::NotInitialized)?;

        let client = token::Client::new(&env, &token_addr);
        client.transfer(&env.current_contract_address(), &recipient, &amount);

        let record = RefundRecord {
            amount,
            recipient: recipient.clone(),
            ledger: env.ledger().sequence(),
        };

        env.storage().persistent().set(&DataKey::Refund(payment_ref.clone()), &record);

        let topics = (Symbol::new(&env, "refund_event"), payment_ref);
        env.events().publish(
            topics,
            (record.amount, record.recipient, record.ledger),
        );

        Ok(())
    }

    /// Withdraws merchant float tokens from the vault back to the merchant's address.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `amount` - The token amount to withdraw, in atomic units (must be $> 0$).
    /// * `to` - The destination address to receive the withdrawn tokens (must be the merchant admin).
    ///
    /// # Errors
    /// * `RefundVaultError::NotInitialized` - If the contract has not been initialized.
    /// * `RefundVaultError::Paused` - If the vault is currently paused.
    /// * `RefundVaultError::InvalidAmount` - If `amount` is $\le 0$.
    /// * `RefundVaultError::Unauthorized` - If `to` is not the merchant admin.
    ///
    /// # Authorization
    /// Requires authorization from the merchant admin (`Admin`).
    pub fn withdraw(env: Env, amount: i128, to: Address) -> Result<(), RefundVaultError> {
        let paused: bool = env.storage().persistent().get(&DataKey::Paused).unwrap_or(false);
        if paused {
            return Err(RefundVaultError::Paused);
        }
        if amount <= 0 {
            return Err(RefundVaultError::InvalidAmount);
        }

        let admin: Address = env
            .storage()
            .persistent()
            .get(&DataKey::Admin)
            .ok_or(RefundVaultError::NotInitialized)?;

        if to != admin {
            return Err(RefundVaultError::Unauthorized);
        }
        admin.require_auth();

        let token_addr: Address = env
            .storage()
            .persistent()
            .get(&DataKey::Token)
            .ok_or(RefundVaultError::NotInitialized)?;

        let client = token::Client::new(&env, &token_addr);
        client.transfer(&env.current_contract_address(), &to, &amount);

        let topics = (Symbol::new(&env, "withdraw_event"), to.clone());
        env.events().publish(topics, amount);

        Ok(())
    }

    /// Updates the refund time window policy.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `ledgers` - The new refund window duration in ledgers. Setting `ledgers` to `0` disables expiry entirely.
    ///
    /// # Errors
    /// * `RefundVaultError::NotInitialized` - If the contract has not been initialized.
    /// * `RefundVaultError::Unauthorized` - If the caller is not the merchant admin.
    ///
    /// # Authorization
    /// Requires authorization from the merchant admin (`Admin`).
    pub fn set_refund_window(env: Env, ledgers: u32) -> Result<(), RefundVaultError> {
        let admin: Address = env
            .storage()
            .persistent()
            .get(&DataKey::Admin)
            .ok_or(RefundVaultError::NotInitialized)?;
        admin.require_auth();

        env.storage().persistent().set(&DataKey::RefundWindow, &ledgers);
        Ok(())
    }

    /// Looks up a refund record by its payment reference hash.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `payment_ref` - The 32-byte payment reference hash.
    ///
    /// # Returns
    /// Returns `Some(RefundRecord)` if a refund was executed for the given reference, or `None` otherwise.
    ///
    /// # Errors
    /// * `RefundVaultError::NotInitialized` - If the contract has not been initialized.
    ///
    /// # Authorization
    /// Read-only; requires no authorization.
    pub fn get_refund(env: Env, payment_ref: BytesN<32>) -> Result<Option<RefundRecord>, RefundVaultError> {
        if !env.storage().persistent().has(&DataKey::Admin) {
            return Err(RefundVaultError::NotInitialized);
        }
        let record = env.storage().persistent().get(&DataKey::Refund(payment_ref));
        Ok(record)
    }

    /// Pauses vault operations (deposits, refunds, and withdrawals) for emergency stops.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    ///
    /// # Errors
    /// * `RefundVaultError::NotInitialized` - If the contract has not been initialized.
    /// * `RefundVaultError::Unauthorized` - If the caller is not the merchant admin.
    ///
    /// # Authorization
    /// Requires authorization from the merchant admin (`Admin`).
    pub fn pause(env: Env) -> Result<(), RefundVaultError> {
        let admin: Address = env
            .storage()
            .persistent()
            .get(&DataKey::Admin)
            .ok_or(RefundVaultError::NotInitialized)?;
        admin.require_auth();

        env.storage().persistent().set(&DataKey::Paused, &true);
        Ok(())
    }

    /// Resumes paused vault operations.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    ///
    /// # Errors
    /// * `RefundVaultError::NotInitialized` - If the contract has not been initialized.
    /// * `RefundVaultError::Unauthorized` - If the caller is not the merchant admin.
    ///
    /// # Authorization
    /// Requires authorization from the merchant admin (`Admin`).
    pub fn unpause(env: Env) -> Result<(), RefundVaultError> {
        let admin: Address = env
            .storage()
            .persistent()
            .get(&DataKey::Admin)
            .ok_or(RefundVaultError::NotInitialized)?;
        admin.require_auth();

        env.storage().persistent().set(&DataKey::Paused, &false);
        Ok(())
    }

    /// Extends the TTL (Time-To-Live) of a refund record to prevent state archival.
    ///
    /// This function is intentionally publicly callable by anyone, allowing agents or integrators
    /// to sponsor or maintain storage liveness for completed refund records.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `payment_ref` - The 32-byte payment reference hash of the refund record whose TTL should be extended.
    ///
    /// # Errors
    /// * `RefundVaultError::RefundNotFound` - If no refund record exists for the given `payment_ref`.
    ///
    /// # Authorization
    /// Publicly callable; requires no authorization.
    pub fn extend_refund_ttl(env: Env, payment_ref: BytesN<32>) -> Result<(), RefundVaultError> {
        if !env.storage().persistent().has(&DataKey::Refund(payment_ref.clone())) {
            return Err(RefundVaultError::RefundNotFound);
        }
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Refund(payment_ref), 500000, 500000);
        Ok(())
    }

    /// Initiates a transfer of the merchant admin role to a new address.
    ///
    /// The pending admin must subsequently call `accept_admin` to complete the transfer.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `new_admin` - The address of the proposed new admin.
    ///
    /// # Errors
    /// * `RefundVaultError::NotInitialized` - If the contract has not been initialized.
    /// * `RefundVaultError::Unauthorized` - If the caller is not the current merchant admin.
    ///
    /// # Authorization
    /// Requires authorization from the current merchant admin (`Admin`).
    pub fn transfer_admin(env: Env, new_admin: Address) -> Result<(), RefundVaultError> {
        let admin: Address = env
            .storage()
            .persistent()
            .get(&DataKey::Admin)
            .ok_or(RefundVaultError::NotInitialized)?;
        admin.require_auth();

        env.storage().persistent().set(&DataKey::PendingAdmin, &new_admin);
        Ok(())
    }

    /// Accepts a pending admin role transfer.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    ///
    /// # Errors
    /// * `RefundVaultError::NotInitialized` - If the contract has not been initialized.
    /// * `RefundVaultError::NoPendingAdmin` - If no pending admin transfer has been initiated.
    /// * `RefundVaultError::Unauthorized` - If the caller is not the designated pending admin.
    ///
    /// # Authorization
    /// Requires authorization from the pending admin address (`PendingAdmin`).
    pub fn accept_admin(env: Env) -> Result<(), RefundVaultError> {
        let pending_admin: Address = env
            .storage()
            .persistent()
            .get(&DataKey::PendingAdmin)
            .ok_or(RefundVaultError::NoPendingAdmin)?;
        pending_admin.require_auth();

        env.storage().persistent().set(&DataKey::Admin, &pending_admin);
        env.storage().persistent().remove(&DataKey::PendingAdmin);
        Ok(())
    }

    /// Cancels a pending admin role transfer.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    ///
    /// # Errors
    /// * `RefundVaultError::NotInitialized` - If the contract has not been initialized.
    /// * `RefundVaultError::NoPendingAdmin` - If there is no pending admin transfer to cancel.
    /// * `RefundVaultError::Unauthorized` - If the caller is not the current merchant admin.
    ///
    /// # Authorization
    /// Requires authorization from the current merchant admin (`Admin`).
    pub fn cancel_admin_transfer(env: Env) -> Result<(), RefundVaultError> {
        let admin: Address = env
            .storage()
            .persistent()
            .get(&DataKey::Admin)
            .ok_or(RefundVaultError::NotInitialized)?;
        admin.require_auth();

        if !env.storage().persistent().has(&DataKey::PendingAdmin) {
            return Err(RefundVaultError::NoPendingAdmin);
        }
        env.storage().persistent().remove(&DataKey::PendingAdmin);
        Ok(())
    }
}
