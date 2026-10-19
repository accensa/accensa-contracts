use crate::Error;
use soroban_sdk::{contracttype, Env};

/// Instance-storage key for the reentrancy lock: `Lock` holds `true` while a guarded entry point runs, and is absent (unlocked) before the first `acquire`.
#[contracttype]
pub enum ReentrancyDataKey {
    Lock,
}

/// Reentrancy guard over the single `ReentrancyDataKey::Lock` instance-storage flag, shared by every guarded entry point of one contract.
pub struct ReentrancyGuard;

impl ReentrancyGuard {
    /// Takes the lock for a guarded entry point; call it before that entry point's first external call. Returns `Error::ReentrancyBlocked` if the lock is already held.
    pub fn acquire(env: &Env) -> Result<(), Error> {
        let is_locked: bool = env
            .storage()
            .instance()
            .get(&ReentrancyDataKey::Lock)
            .unwrap_or(false);
        if is_locked {
            return Err(Error::ReentrancyBlocked);
        }
        env.storage()
            .instance()
            .set(&ReentrancyDataKey::Lock, &true);
        Ok(())
    }

    /// Clears the lock unconditionally; call it after every successful `acquire`, including on the error paths that follow it.
    pub fn release(env: &Env) {
        env.storage()
            .instance()
            .set(&ReentrancyDataKey::Lock, &false);
    }
}
