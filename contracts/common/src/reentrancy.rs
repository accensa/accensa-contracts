use crate::Error;
use soroban_sdk::{contracttype, Env};

#[contracttype]
pub enum ReentrancyDataKey {
    Lock,
}

pub struct ReentrancyGuard;

impl ReentrancyGuard {
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

    pub fn release(env: &Env) {
        env.storage()
            .instance()
            .set(&ReentrancyDataKey::Lock, &false);
    }
}
