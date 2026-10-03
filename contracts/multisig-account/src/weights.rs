//! Weighted signer thresholds (issue #434).
//!
//! A plain `threshold`-of-`signers` account treats every signer as equal. Real
//! organizations do not: a lead developer's approval should count for more
//! than a new contributor's. This module lets governance assign each signer an
//! integer [`weight`](signer_weight) and changes `__check_auth` to compare the
//! **aggregate weight** of the attached approvers against the account's
//! threshold rather than the raw number of approvers.
//!
//! # Model
//!
//! - **Mapping `Signer -> Weight`.** Every registered signer has a weight in
//!   persistent storage ([`DataKey::SignerWeight`]); the default, and the
//!   value every signer gets at construction time, is
//!   [`DEFAULT_SIGNER_WEIGHT`] (`1`). An account that never touches weights
//!   therefore behaves exactly as it did before: aggregate weight equals the
//!   number of distinct approvers.
//! - **Weighted tally.** `__check_auth` sums the weights of the attached
//!   delegated signers ([`tally_weight`]) and admits the call when the total
//!   is `>= threshold`. An unknown delegate is still rejected outright.
//! - **Invariant `total_signers_weight >= threshold`.** The account must
//!   always retain enough weight to be able to authorize anything at all.
//!   Every mutation here checks the *would-be* aggregate weight before writing
//!   and refuses with [`Error::TotalWeightBelowThreshold`] otherwise, so the
//!   account can never be wedged into a state where no set of approvers can
//!   reach the threshold.
//! - **Governance = the account's own authorization.** Updating a weight is a
//!   privileged operation: it requires `current_contract_address().require_auth()`,
//!   i.e. the current weighted threshold of signers must approve it, exactly as
//!   for signer rotation and the daily-limit changes.
//!
//! Weights are `u32` and deliberately bounded away from zero: a zero-weight
//! signer would be a registered delegate that can never contribute, which is
//! only ever a foot-gun. Use [`remove_signer`] to drop a signer entirely.

use soroban_sdk::{contractevent, Address, Env, Vec};

use crate::{DataKey, Error};

/// Weight given to a signer that has no explicit weight stored, and to every
/// signer registered through the constructor or signer rotation.
pub const DEFAULT_SIGNER_WEIGHT: u32 = 1;

/// The registered signers, in registration order.
///
/// Soroban storage cannot be iterated, so the aggregate weight needs a
/// materialized list. The list is authoritative for which signers exist for
/// the purpose of [`total_weight`]; the per-signer
/// [`DataKey::Signer`] marker remains the check used by `__check_auth`.
pub fn signer_list(env: &Env) -> Vec<Address> {
    env.storage()
        .persistent()
        .get(&DataKey::SignerList)
        .unwrap_or_else(|| Vec::new(env))
}

pub(crate) fn store_signer_list(env: &Env, signers: &Vec<Address>) {
    env.storage()
        .persistent()
        .set(&DataKey::SignerList, signers);
}

/// A registered signer's weight ([`DEFAULT_SIGNER_WEIGHT`] when unset).
pub fn signer_weight(env: &Env, signer: &Address) -> u32 {
    env.storage()
        .persistent()
        .get(&DataKey::SignerWeight(signer.clone()))
        .unwrap_or(DEFAULT_SIGNER_WEIGHT)
}

pub(crate) fn store_signer_weight(env: &Env, signer: &Address, weight: u32) {
    env.storage()
        .persistent()
        .set(&DataKey::SignerWeight(signer.clone()), &weight);
}

pub(crate) fn clear_signer_weight(env: &Env, signer: &Address) {
    env.storage()
        .persistent()
        .remove(&DataKey::SignerWeight(signer.clone()));
}

/// Aggregate weight of every registered signer.
///
/// Saturates rather than wrapping: the value is only ever compared against a
/// `u32` threshold, and a saturated total can only fail *open*, which the
/// per-mutation invariant checks below make unreachable anyway.
pub fn total_weight(env: &Env) -> u32 {
    let mut total: u32 = 0;
    for signer in signer_list(env).iter() {
        total = total.saturating_add(signer_weight(env, &signer));
    }
    total
}

/// Aggregate weight of the distinct approving signers in `delegates`.
///
/// Rejects any delegate that is not a registered signer with
/// [`Error::UnknownSigner`], so a tally never credits an unregistered address.
pub fn tally_weight(env: &Env, delegates: &Vec<Address>) -> Result<u32, Error> {
    let mut total: u32 = 0;
    for signer in delegates.iter() {
        if !env
            .storage()
            .persistent()
            .has(&DataKey::Signer(signer.clone()))
        {
            return Err(Error::UnknownSigner);
        }
        total = total.saturating_add(signer_weight(env, &signer));
    }
    Ok(total)
}

/// The account's current threshold.
fn threshold(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&DataKey::Threshold)
        .unwrap_or(1)
}

/// Would-be aggregate weight after moving `signer` from `previous` to
/// `new_weight`? Refuses values that do not fit a `u32`.
fn projected_total(total: u32, previous: u32, new_weight: u32) -> Result<u32, Error> {
    total
        .checked_sub(previous)
        .and_then(|rest| rest.checked_add(new_weight))
        .ok_or(Error::InvalidWeight)
}

/// Governance-only: set `signer`'s voting weight to `weight`.
///
/// Requires the account's own authorization (the current weighted threshold).
/// Refuses a zero weight with [`Error::InvalidWeight`], an unregistered signer
/// with [`Error::UnknownSigner`], and any change that would leave
/// `total_signers_weight < threshold` with
/// [`Error::TotalWeightBelowThreshold`].
///
/// # Events emitted on success
/// - [`SignerWeightSet`]
pub fn set_signer_weight(env: &Env, signer: Address, weight: u32) -> Result<(), Error> {
    env.current_contract_address().require_auth();

    if weight == 0 {
        return Err(Error::InvalidWeight);
    }
    if !env
        .storage()
        .persistent()
        .has(&DataKey::Signer(signer.clone()))
    {
        return Err(Error::UnknownSigner);
    }

    let previous = signer_weight(env, &signer);
    let total = total_weight(env);
    let next_total = projected_total(total, previous, weight)?;
    if next_total < threshold(env) {
        return Err(Error::TotalWeightBelowThreshold);
    }

    store_signer_weight(env, &signer, weight);

    SignerWeightSet {
        signer,
        previous_weight: previous,
        new_weight: weight,
        total_weight: next_total,
    }
    .publish(env);
    Ok(())
}

/// Governance-only: register `signer` with an explicit `weight`.
///
/// Adding weight can never violate `total >= threshold`, so the only errors
/// are a zero weight ([`Error::InvalidWeight`]) and an address that is already
/// a signer ([`Error::SignerAlreadyRegistered`]). Use
/// [`set_signer_weight`] to change an existing signer's weight.
///
/// # Events emitted on success
/// - [`SignerAdded`]
pub fn add_signer(env: &Env, signer: Address, weight: u32) -> Result<(), Error> {
    env.current_contract_address().require_auth();

    if weight == 0 {
        return Err(Error::InvalidWeight);
    }
    if env
        .storage()
        .persistent()
        .has(&DataKey::Signer(signer.clone()))
    {
        return Err(Error::SignerAlreadyRegistered);
    }

    env.storage()
        .persistent()
        .set(&DataKey::Signer(signer.clone()), &());
    store_signer_weight(env, &signer, weight);

    let mut list = signer_list(env);
    list.push_back(signer.clone());
    store_signer_list(env, &list);

    SignerAdded {
        signer,
        weight,
        total_weight: total_weight(env),
    }
    .publish(env);
    Ok(())
}

/// Governance-only: drop `signer` from the account.
///
/// Refuses with [`Error::TotalWeightBelowThreshold`] when removing the
/// signer's weight would leave the account unable to reach its threshold;
/// lower the threshold first (via
/// [`rotate_signers_and_threshold`](crate::MultisigAccount::rotate_signers_and_threshold)).
///
/// # Events emitted on success
/// - [`SignerRemoved`]
pub fn remove_signer(env: &Env, signer: Address) -> Result<(), Error> {
    env.current_contract_address().require_auth();

    if !env
        .storage()
        .persistent()
        .has(&DataKey::Signer(signer.clone()))
    {
        return Err(Error::UnknownSigner);
    }

    let removed = signer_weight(env, &signer);
    let total = total_weight(env);
    let next_total = total.checked_sub(removed).ok_or(Error::InvalidWeight)?;
    if next_total < threshold(env) {
        return Err(Error::TotalWeightBelowThreshold);
    }

    env.storage()
        .persistent()
        .remove(&DataKey::Signer(signer.clone()));
    clear_signer_weight(env, &signer);

    let mut kept: Vec<Address> = Vec::new(env);
    for entry in signer_list(env).iter() {
        if entry != signer {
            kept.push_back(entry);
        }
    }
    store_signer_list(env, &kept);

    SignerRemoved {
        signer,
        total_weight: next_total,
    }
    .publish(env);
    Ok(())
}

/// Emitted when a signer's weight changes.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignerWeightSet {
    #[topic]
    pub signer: Address,
    pub previous_weight: u32,
    pub new_weight: u32,
    /// Aggregate signer weight after the change.
    pub total_weight: u32,
}

/// Emitted when a weighted signer is added.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignerAdded {
    #[topic]
    pub signer: Address,
    pub weight: u32,
    /// Aggregate signer weight after the change.
    pub total_weight: u32,
}

/// Emitted when a signer is removed.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignerRemoved {
    #[topic]
    pub signer: Address,
    /// Aggregate signer weight after the change.
    pub total_weight: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutils::make_auth_entry_with_nonce;
    use crate::{MultisigAccount, MultisigAccountClient};
    use soroban_sdk::{
        testutils::Address as _,
        token::{StellarAssetClient, TokenClient},
        vec, Address, Env, IntoVal, Val,
    };

    /// 3-signer, threshold-3 account with `(s1, s2, s3)` weights `(3, 1, 1)`:
    /// s1 alone reaches the threshold, s2/s3 alone do not.
    struct Fx {
        env: Env,
        client: MultisigAccountClient<'static>,
        account: Address,
        token: Address,
        s1: Address,
        s2: Address,
        s3: Address,
    }

    fn setup() -> Fx {
        let env = Env::default();
        env.mock_all_auths();
        let (s1, s2, s3) = (
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
        );
        let account = env.register(
            MultisigAccount,
            (vec![&env, s1.clone(), s2.clone(), s3.clone()], 3u32),
        );
        let client = MultisigAccountClient::new(&env, &account);

        let token = env
            .register_stellar_asset_contract_v2(Address::generate(&env))
            .address();
        StellarAssetClient::new(&env, &token).mint(&account, &10_000);

        client.set_signer_weight(&s1, &3);
        Fx {
            env,
            client,
            account,
            token,
            s1,
            s2,
            s3,
        }
    }

    impl Fx {
        /// Attempt a token transfer authorized by `signers` (by weight), with
        /// real `__check_auth` verification.
        fn transfer(&self, signers: &[Address], amount: i128) -> bool {
            let to = Address::generate(&self.env);
            let args: [Val; 3] = [
                self.account.clone().into_val(&self.env),
                to.clone().into_val(&self.env),
                amount.into_val(&self.env),
            ];
            let entry = make_auth_entry_with_nonce(
                &self.env,
                &self.account,
                &self.token,
                "transfer",
                &args,
                signers,
                1,
            );
            self.env.set_auths(&[entry]);
            TokenClient::new(&self.env, &self.token)
                .try_transfer(&self.account, &to, &amount)
                .is_ok()
        }
    }

    #[test]
    fn weights_default_to_one_and_total_matches_the_signer_count() {
        let env = Env::default();
        env.mock_all_auths();
        let s = Address::generate(&env);
        let id = env.register(MultisigAccount, (vec![&env, s.clone()], 1u32));
        let client = MultisigAccountClient::new(&env, &id);

        assert_eq!(client.get_signer_weight(&s), DEFAULT_SIGNER_WEIGHT);
        assert_eq!(client.get_total_weight(), 1);
    }

    #[test]
    fn setting_a_weight_updates_the_mapping_and_the_total() {
        let fx = setup();
        assert_eq!(fx.client.get_signer_weight(&fx.s1), 3);
        assert_eq!(fx.client.get_total_weight(), 5);

        fx.client.set_signer_weight(&fx.s2, &4);
        assert_eq!(fx.client.get_signer_weight(&fx.s2), 4);
        assert_eq!(fx.client.get_total_weight(), 8);
        assert_eq!(
            fx.client.get_signer_weight(&fx.s3),
            1,
            "untouched signers keep their weight"
        );
    }

    #[test]
    fn a_zero_weight_is_rejected() {
        let fx = setup();
        assert_eq!(
            fx.client.try_set_signer_weight(&fx.s2, &0),
            Err(Ok(Error::InvalidWeight))
        );
        assert_eq!(
            fx.client.try_add_signer(&Address::generate(&fx.env), &0),
            Err(Ok(Error::InvalidWeight))
        );
    }

    #[test]
    fn updating_an_unknown_signer_is_rejected() {
        let fx = setup();
        let outsider = Address::generate(&fx.env);
        assert_eq!(
            fx.client.try_set_signer_weight(&outsider, &2),
            Err(Ok(Error::UnknownSigner))
        );
    }

    #[test]
    fn a_change_that_would_drop_the_total_below_threshold_is_rejected() {
        let fx = setup();
        // Threshold 3, total 5. Lowering s1 from 3 to 1 would leave total 3,
        // still >= 3 — allowed.
        fx.client.set_signer_weight(&fx.s1, &1);
        assert_eq!(fx.client.get_total_weight(), 3);

        // Now raise the threshold to 3 with total 3, and try to remove a
        // signer: that would leave total 2 < 3.
        assert_eq!(
            fx.client.try_remove_signer(&fx.s3),
            Err(Ok(Error::TotalWeightBelowThreshold))
        );
        // The rejected removal changed nothing.
        assert_eq!(fx.client.get_total_weight(), 3);
        assert!(fx.client.is_signer(&fx.s3));
    }

    #[test]
    fn adding_a_signer_with_a_weight_raises_the_total() {
        let env = Env::default();
        env.mock_all_auths();
        let s1 = Address::generate(&env);
        let id = env.register(MultisigAccount, (vec![&env, s1.clone()], 1u32));
        let client = MultisigAccountClient::new(&env, &id);
        let extra = Address::generate(&env);

        client.add_signer(&extra, &4);
        assert!(client.is_signer(&extra));
        assert_eq!(client.get_signer_weight(&extra), 4);
        assert_eq!(client.get_total_weight(), 5);

        // Duplicate addition is refused.
        assert_eq!(
            client.try_add_signer(&extra, &1),
            Err(Ok(Error::SignerAlreadyRegistered))
        );

        client.remove_signer(&extra);
        assert!(!client.is_signer(&extra));
        assert_eq!(client.get_total_weight(), 1);
    }

    #[test]
    fn a_heavy_signer_alone_meets_the_weight_threshold() {
        let fx = setup();
        // s1 has weight 3 == threshold: approved with a single signature.
        assert!(fx.transfer(core::slice::from_ref(&fx.s1), 500));
    }

    #[test]
    fn a_light_signer_alone_is_below_the_weight_threshold() {
        let fx = setup();
        // s2 has weight 1 < threshold 3 and no daily allowance is configured.
        assert!(!fx.transfer(core::slice::from_ref(&fx.s2), 500));
    }

    #[test]
    fn light_signers_can_combine_to_reach_the_threshold() {
        let fx = setup();
        // s2 + s3 = 1 + 1 = 2 < 3: still short.
        assert!(!fx.transfer(&[fx.s2.clone(), fx.s3.clone()], 100));
        // s1 + s2 = 3 + 1 = 4 >= 3.
        assert!(fx.transfer(&[fx.s1.clone(), fx.s2.clone()], 100));
    }
}
