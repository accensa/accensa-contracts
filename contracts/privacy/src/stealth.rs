//! Dual-key stealth-address registry for vault deposits (issue #439).
//!
//! A buyer funds an escrow vault without publishing the recipient's main
//! public key next to the purchase. The scheme is the dual-key stealth
//! address protocol on **BN254 G1**, using the host's curve operations:
//!
//! | name | meaning |
//! |---|---|
//! | `s`, `S = s*G` | the recipient's spend secret and public key |
//! | `v`, `V = v*G` | the recipient's view secret and public key |
//! | `r`, `R = r*G` | the buyer's ephemeral secret and public key |
//! | `t = H(r*V)` | the tweak both sides derive from the ECDH shared secret |
//! | `P = S + t*G` | the one-time stealth public key that holds the deposit |
//!
//! 1. The recipient registers `(S, V)` with [`Privacy::register_stealth_keys`].
//! 2. The buyer reads them, picks a fresh `r`, derives `(R, P)` off chain with
//!    [`derive_stealth_address`] and calls [`Privacy::stealth_deposit`]. The
//!    tokens move into this contract and an event carries `R` and `P`.
//! 3. The recipient scans those events: for each `R` they compute
//!    `t = H(v*R)` and `P' = S + t*G` ([`scan_stealth_address`]). A match
//!    means the deposit is theirs, and its private key is `p = s + t`
//!    ([`stealth_private_key`]).
//! 4. The recipient spends with [`Privacy::stealth_claim`]: a Schnorr proof of
//!    knowledge of `p`, bound to the deposit and to the address that receives
//!    the funds. Only someone holding `s` and `v` (or `p`) can produce it.
//!
//! Only the buyer knows `r`, so `r` is never passed to the contract: the
//! derivation helpers are ordinary library functions for off-chain code and
//! tests. An observer sees `S`, `V`, `R` and `P` but cannot tell that `P`
//! belongs to `S` without `v` or `r`.
//!
//! # Hashing to a scalar
//!
//! `H` is two domain-separated SHA-256 digests read as one 512-bit number and
//! reduced modulo the group order, so the result is uniform up to a
//! negligible bias. See [`hash_to_scalar`].
//!
//! # Limits
//!
//! The deposit amount is visible (it is a token transfer). What is hidden is
//! the link between the deposit and the recipient's registered keys.

use soroban_sdk::{
    contracterror, contractevent, contractimpl, contracttype,
    crypto::bn254::{Bn254Fr, Bn254G1Affine},
    token,
    xdr::ToXdr,
    Address, Bytes, BytesN, Env,
};

use crate::{Privacy, PrivacyArgs, PrivacyClient};

/// Domain separator of the ECDH tweak.
const TWEAK_DOMAIN: &[u8] = b"accensa/stealth/tweak/v1";
/// Domain separator of the claim challenge.
const CLAIM_DOMAIN: &[u8] = b"accensa/stealth/claim/v1";
/// Domain separator of the deposit commitment.
const COMMITMENT_DOMAIN: &[u8] = b"accensa/stealth/commitment/v1";

/// Errors of the stealth registry. Local to this module for the same reason
/// as the contract's own `Error`: it keeps unrelated specs small.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum StealthError {
    /// The account has already registered keys.
    KeysAlreadyRegistered = 1,
    /// A key or ephemeral key is not a valid, non-zero BN254 G1 point.
    InvalidPoint = 2,
    /// The amount is not strictly positive.
    InvalidAmount = 3,
    /// A deposit for this stealth key and ephemeral key already exists.
    DuplicateDeposit = 4,
    /// There is no unclaimed deposit under this commitment.
    DepositNotFound = 5,
    /// The Schnorr proof does not verify.
    InvalidSignature = 6,
}

/// Storage keys of this module.
#[contracttype]
pub enum StealthKey {
    /// Persistent: the registered keys of an account.
    Keys(Address),
    /// Persistent: an unclaimed deposit, by commitment.
    Deposit(BytesN<32>),
}

/// A recipient's published keys, as 64-byte BN254 G1 encodings.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StealthKeys {
    pub spend_pub: BytesN<64>,
    pub view_pub: BytesN<64>,
}

/// An unclaimed deposit.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StealthDeposit {
    /// The one-time stealth public key `P` that controls the deposit.
    pub stealth_pub: BytesN<64>,
    /// The buyer's ephemeral public key `R`, for recipients to scan with.
    pub ephemeral_pub: BytesN<64>,
    pub token: Address,
    pub amount: i128,
}

/// Emitted when keys are registered.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StealthKeysRegistered {
    #[topic]
    pub owner: Address,
    pub spend_pub: BytesN<64>,
    pub view_pub: BytesN<64>,
}

/// Emitted for every deposit. Recipients scan these: `ephemeral_pub` is the
/// input of the scan and `commitment` is the handle used to claim.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StealthDeposited {
    #[topic]
    pub commitment: BytesN<32>,
    pub ephemeral_pub: BytesN<64>,
    pub stealth_pub: BytesN<64>,
    pub token: Address,
    pub amount: i128,
}

/// Emitted when a deposit is claimed.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StealthClaimed {
    #[topic]
    pub commitment: BytesN<32>,
    pub recipient: Address,
}

// ── curve helpers ───────────────────────────────────────────────────────────

/// The BN254 G1 generator `(1, 2)`.
pub fn generator(env: &Env) -> Bn254G1Affine {
    let mut bytes = [0u8; 64];
    bytes[31] = 1;
    bytes[63] = 2;
    Bn254G1Affine::from_bytes(BytesN::from_array(env, &bytes))
}

/// A valid, non-zero point: on the curve and not the point at infinity.
fn is_valid_point(env: &Env, point: &BytesN<64>) -> bool {
    if point.to_array().iter().all(|&b| b == 0) {
        return false;
    }
    env.crypto()
        .bn254()
        .g1_is_on_curve(&Bn254G1Affine::from_bytes(point.clone()))
}

fn to_point(point: &BytesN<64>) -> Bn254G1Affine {
    Bn254G1Affine::from_bytes(point.clone())
}

fn from_point(point: &Bn254G1Affine) -> BytesN<64> {
    point.to_bytes()
}

/// A 512-bit hash reduced to a scalar: `(hi * 2^256 + lo) mod r` for two
/// domain-separated SHA-256 digests `hi` and `lo`.
pub fn hash_to_scalar(env: &Env, domain: &[u8], data: &Bytes) -> Bn254Fr {
    let digest = |tag: u8| {
        let mut input = Bytes::from_slice(env, domain);
        input.push_back(tag);
        input.append(data);
        Bn254Fr::from_bytes(env.crypto().sha256(&input).to_bytes())
    };
    let hi = digest(1);
    let lo = digest(2);

    // 2^256 mod r, as (2^128)^2 in the field.
    let mut two128 = [0u8; 32];
    two128[15] = 1;
    let two128 = Bn254Fr::from_bytes(BytesN::from_array(env, &two128));
    let two256 = two128.clone() * two128;
    hi * two256 + lo
}

/// The tweak `t = H(shared)` for a shared point `shared = r*V = v*R`.
fn tweak(env: &Env, shared: &Bn254G1Affine) -> Bn254Fr {
    hash_to_scalar(
        env,
        TWEAK_DOMAIN,
        &Bytes::from_slice(env, &shared.to_array()),
    )
}

/// The buyer's side: from the recipient's keys and a fresh secret `r`,
/// returns `(R, P)`, the ephemeral public key to publish and the one-time
/// stealth public key to deposit to.
///
/// `ephemeral_secret` is a secret. Call this off chain; never pass `r` to a
/// contract function.
pub fn derive_stealth_address(
    env: &Env,
    spend_pub: &BytesN<64>,
    view_pub: &BytesN<64>,
    ephemeral_secret: &Bn254Fr,
) -> (BytesN<64>, BytesN<64>) {
    let g = generator(env);
    let ephemeral_pub = g.clone() * ephemeral_secret.clone();
    let shared = to_point(view_pub) * ephemeral_secret.clone();
    let stealth_pub = to_point(spend_pub) + g * tweak(env, &shared);
    (from_point(&ephemeral_pub), from_point(&stealth_pub))
}

/// The recipient's side: the stealth public key a deposit with ephemeral key
/// `R` would use for this recipient. A deposit is theirs when this equals its
/// `stealth_pub`.
pub fn scan_stealth_address(
    env: &Env,
    spend_pub: &BytesN<64>,
    view_secret: &Bn254Fr,
    ephemeral_pub: &BytesN<64>,
) -> BytesN<64> {
    let shared = to_point(ephemeral_pub) * view_secret.clone();
    let stealth_pub = to_point(spend_pub) + generator(env) * tweak(env, &shared);
    from_point(&stealth_pub)
}

/// The private key `p = s + t` of a deposit that belongs to the recipient.
pub fn stealth_private_key(
    env: &Env,
    spend_secret: &Bn254Fr,
    view_secret: &Bn254Fr,
    ephemeral_pub: &BytesN<64>,
) -> Bn254Fr {
    let shared = to_point(ephemeral_pub) * view_secret.clone();
    spend_secret.clone() + tweak(env, &shared)
}

/// The handle of a deposit: a hash of both public keys, so two deposits can
/// never share one and a claim names exactly one of them.
pub fn deposit_commitment(
    env: &Env,
    stealth_pub: &BytesN<64>,
    ephemeral_pub: &BytesN<64>,
) -> BytesN<32> {
    let mut input = Bytes::from_slice(env, COMMITMENT_DOMAIN);
    input.append(&Bytes::from_slice(env, &stealth_pub.to_array()));
    input.append(&Bytes::from_slice(env, &ephemeral_pub.to_array()));
    env.crypto().sha256(&input).to_bytes()
}

/// The Schnorr challenge of a claim: binds the stealth key, the nonce point,
/// the deposit and the account that receives the funds, so a proof cannot be
/// replayed for another deposit or redirected to another account.
pub fn claim_challenge(
    env: &Env,
    stealth_pub: &BytesN<64>,
    nonce_point: &BytesN<64>,
    commitment: &BytesN<32>,
    recipient: &Address,
) -> Bn254Fr {
    let mut data = Bytes::from_slice(env, &stealth_pub.to_array());
    data.append(&Bytes::from_slice(env, &nonce_point.to_array()));
    data.append(&Bytes::from_slice(env, &commitment.to_array()));
    data.append(&recipient.clone().to_xdr(env));
    hash_to_scalar(env, CLAIM_DOMAIN, &data)
}

#[contractimpl]
impl Privacy {
    /// Publish the caller's spend and view public keys. Once per account.
    ///
    /// # Errors
    /// - [`StealthError::InvalidPoint`] if either key is not a valid non-zero
    ///   BN254 G1 point.
    /// - [`StealthError::KeysAlreadyRegistered`] on a second registration.
    pub fn register_stealth_keys(
        env: Env,
        owner: Address,
        spend_pub: BytesN<64>,
        view_pub: BytesN<64>,
    ) -> Result<(), StealthError> {
        owner.require_auth();
        if !is_valid_point(&env, &spend_pub) || !is_valid_point(&env, &view_pub) {
            return Err(StealthError::InvalidPoint);
        }
        let key = StealthKey::Keys(owner.clone());
        if env.storage().persistent().has(&key) {
            return Err(StealthError::KeysAlreadyRegistered);
        }
        env.storage().persistent().set(
            &key,
            &StealthKeys {
                spend_pub: spend_pub.clone(),
                view_pub: view_pub.clone(),
            },
        );
        let ttl = accensa_common::storage::DEFAULT_TTL_BUMP.min(env.storage().max_ttl());
        env.storage().persistent().extend_ttl(&key, ttl, ttl);

        StealthKeysRegistered {
            owner,
            spend_pub,
            view_pub,
        }
        .publish(&env);
        Ok(())
    }

    /// The keys `owner` registered, for buyers to derive a stealth address from.
    pub fn get_stealth_keys(env: Env, owner: Address) -> Option<StealthKeys> {
        env.storage().persistent().get(&StealthKey::Keys(owner))
    }

    /// Fund a vault at a stealth address. The tokens move from `depositor` to
    /// this contract, and the deposit is recorded under a commitment to
    /// `(stealth_pub, ephemeral_pub)`. Returns that commitment.
    ///
    /// The contract cannot check that `stealth_pub` was derived from anyone's
    /// registered keys (that needs `r`), so a wrong value only strands the
    /// buyer's own funds; it can never reach another account.
    ///
    /// # Errors
    /// - [`StealthError::InvalidAmount`] if `amount` is not positive.
    /// - [`StealthError::InvalidPoint`] if either key is not a valid non-zero
    ///   BN254 G1 point.
    /// - [`StealthError::DuplicateDeposit`] if this pair was already used.
    pub fn stealth_deposit(
        env: Env,
        depositor: Address,
        token: Address,
        amount: i128,
        ephemeral_pub: BytesN<64>,
        stealth_pub: BytesN<64>,
    ) -> Result<BytesN<32>, StealthError> {
        depositor.require_auth();
        if amount <= 0 {
            return Err(StealthError::InvalidAmount);
        }
        if !is_valid_point(&env, &ephemeral_pub) || !is_valid_point(&env, &stealth_pub) {
            return Err(StealthError::InvalidPoint);
        }
        let commitment = deposit_commitment(&env, &stealth_pub, &ephemeral_pub);
        let key = StealthKey::Deposit(commitment.clone());
        if env.storage().persistent().has(&key) {
            return Err(StealthError::DuplicateDeposit);
        }

        // Effects before the external transfer.
        env.storage().persistent().set(
            &key,
            &StealthDeposit {
                stealth_pub: stealth_pub.clone(),
                ephemeral_pub: ephemeral_pub.clone(),
                token: token.clone(),
                amount,
            },
        );
        let ttl = accensa_common::storage::DEFAULT_TTL_BUMP.min(env.storage().max_ttl());
        env.storage().persistent().extend_ttl(&key, ttl, ttl);

        token::Client::new(&env, &token).transfer(
            &depositor,
            env.current_contract_address(),
            &amount,
        );

        StealthDeposited {
            commitment: commitment.clone(),
            ephemeral_pub,
            stealth_pub,
            token,
            amount,
        }
        .publish(&env);
        Ok(commitment)
    }

    /// The unclaimed deposit under `commitment`, if any.
    pub fn get_stealth_deposit(env: Env, commitment: BytesN<32>) -> Option<StealthDeposit> {
        env.storage()
            .persistent()
            .get(&StealthKey::Deposit(commitment))
    }

    /// Spend a deposit to `recipient` with a Schnorr proof of knowledge of the
    /// stealth private key `p`: a nonce point `nonce_point = k*G` and
    /// `response = k + c*p`, where `c` is [`claim_challenge`]. The check is
    /// `response*G == nonce_point + c*P`.
    ///
    /// `recipient` authorizes the call and is part of the challenge, so a
    /// proof seen in the mempool cannot be reused to send the funds elsewhere.
    ///
    /// # Errors
    /// - [`StealthError::DepositNotFound`] for an unknown or already claimed
    ///   commitment.
    /// - [`StealthError::InvalidPoint`] if `nonce_point` is not a valid
    ///   non-zero point.
    /// - [`StealthError::InvalidSignature`] if the proof does not verify.
    pub fn stealth_claim(
        env: Env,
        commitment: BytesN<32>,
        recipient: Address,
        nonce_point: BytesN<64>,
        response: BytesN<32>,
    ) -> Result<(), StealthError> {
        recipient.require_auth();
        let key = StealthKey::Deposit(commitment.clone());
        let deposit: StealthDeposit = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(StealthError::DepositNotFound)?;
        if !is_valid_point(&env, &nonce_point) {
            return Err(StealthError::InvalidPoint);
        }

        let challenge = claim_challenge(
            &env,
            &deposit.stealth_pub,
            &nonce_point,
            &commitment,
            &recipient,
        );
        let lhs = generator(&env) * Bn254Fr::from_bytes(response);
        let rhs = to_point(&nonce_point) + to_point(&deposit.stealth_pub) * challenge;
        if lhs.to_bytes() != rhs.to_bytes() {
            return Err(StealthError::InvalidSignature);
        }

        // Effects before the external transfer: the deposit is spent once.
        env.storage().persistent().remove(&key);
        token::Client::new(&env, &deposit.token).transfer(
            &env.current_contract_address(),
            &recipient,
            &deposit.amount,
        );

        StealthClaimed {
            commitment,
            recipient,
        }
        .publish(&env);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use soroban_sdk::{
        testutils::Address as _,
        token::{StellarAssetClient, TokenClient},
    };

    struct Wallet {
        spend_secret: Bn254Fr,
        view_secret: Bn254Fr,
        spend_pub: BytesN<64>,
        view_pub: BytesN<64>,
    }

    fn scalar(env: &Env, n: u64) -> Bn254Fr {
        let mut bytes = [0u8; 32];
        bytes[24..].copy_from_slice(&n.to_be_bytes());
        Bn254Fr::from_bytes(BytesN::from_array(env, &bytes))
    }

    fn public(env: &Env, secret: &Bn254Fr) -> BytesN<64> {
        from_point(&(generator(env) * secret.clone()))
    }

    fn wallet(env: &Env, spend: u64, view: u64) -> Wallet {
        let (spend_secret, view_secret) = (scalar(env, spend), scalar(env, view));
        Wallet {
            spend_pub: public(env, &spend_secret),
            view_pub: public(env, &view_secret),
            spend_secret,
            view_secret,
        }
    }

    struct Setup {
        env: Env,
        client: PrivacyClient<'static>,
        token: Address,
        buyer: Address,
        merchant: Address,
        recipient: Address,
    }

    fn setup() -> Setup {
        let env = Env::default();
        env.mock_all_auths();
        env.cost_estimate().budget().reset_unlimited();
        let admin = Address::generate(&env);
        let contract = env.register(Privacy, (admin,));
        let client = PrivacyClient::new(&env, &contract);

        let sac = env.register_stellar_asset_contract_v2(Address::generate(&env));
        let token = sac.address();
        let buyer = Address::generate(&env);
        StellarAssetClient::new(&env, &token).mint(&buyer, &10_000i128);
        Setup {
            env,
            client,
            token,
            buyer,
            merchant: Address::generate(&Env::default()),
            recipient: Address::generate(&Env::default()),
        }
        .with_accounts()
    }

    impl Setup {
        fn with_accounts(mut self) -> Self {
            self.merchant = Address::generate(&self.env);
            self.recipient = Address::generate(&self.env);
            self
        }

        fn balance(&self, who: &Address) -> i128 {
            TokenClient::new(&self.env, &self.token).balance(who)
        }
    }

    /// Schnorr proof of knowledge of `secret` for `commitment`, paid to
    /// `recipient`, with nonce `k`.
    fn prove(
        s: &Setup,
        secret: &Bn254Fr,
        stealth_pub: &BytesN<64>,
        commitment: &BytesN<32>,
        recipient: &Address,
        k: u64,
    ) -> (BytesN<64>, BytesN<32>) {
        let k = scalar(&s.env, k);
        let nonce_point = public(&s.env, &k);
        let c = claim_challenge(&s.env, stealth_pub, &nonce_point, commitment, recipient);
        let response = k + c * secret.clone();
        (nonce_point, response.to_bytes())
    }

    /// Register a wallet's keys, derive a stealth address with secret `r` and
    /// deposit `amount` to it. Returns `(commitment, R, P)`.
    fn fund(s: &Setup, w: &Wallet, r: u64, amount: i128) -> (BytesN<32>, BytesN<64>, BytesN<64>) {
        let (ephemeral_pub, stealth_pub) =
            derive_stealth_address(&s.env, &w.spend_pub, &w.view_pub, &scalar(&s.env, r));
        let commitment =
            s.client
                .stealth_deposit(&s.buyer, &s.token, &amount, &ephemeral_pub, &stealth_pub);
        (commitment, ephemeral_pub, stealth_pub)
    }

    // ── generation ──────────────────────────────────────────────────────────

    #[test]
    fn the_generator_is_on_the_curve_and_scalar_multiples_are_too() {
        let s = setup();
        let bn = s.env.crypto().bn254();
        assert!(bn.g1_is_on_curve(&generator(&s.env)));
        let w = wallet(&s.env, 11, 22);
        assert!(is_valid_point(&s.env, &w.spend_pub));
        assert!(is_valid_point(&s.env, &w.view_pub));
        assert_ne!(w.spend_pub, w.view_pub);
    }

    #[test]
    fn sender_and_recipient_derive_the_same_stealth_address() {
        let s = setup();
        let w = wallet(&s.env, 11, 22);
        let (ephemeral_pub, stealth_pub) =
            derive_stealth_address(&s.env, &w.spend_pub, &w.view_pub, &scalar(&s.env, 777));

        // The recipient sees only R and their own view key.
        let scanned = scan_stealth_address(&s.env, &w.spend_pub, &w.view_secret, &ephemeral_pub);
        assert_eq!(scanned, stealth_pub);

        // The private key matches the stealth public key: p*G == P.
        let p = stealth_private_key(&s.env, &w.spend_secret, &w.view_secret, &ephemeral_pub);
        assert_eq!(public(&s.env, &p), stealth_pub);
    }

    #[test]
    fn stealth_addresses_are_unlinkable_per_deposit() {
        let s = setup();
        let w = wallet(&s.env, 11, 22);
        let (r1, p1) =
            derive_stealth_address(&s.env, &w.spend_pub, &w.view_pub, &scalar(&s.env, 1));
        let (r2, p2) =
            derive_stealth_address(&s.env, &w.spend_pub, &w.view_pub, &scalar(&s.env, 2));
        assert_ne!(r1, r2);
        assert_ne!(p1, p2);
        // Neither one is the registered spend key.
        assert_ne!(p1, w.spend_pub);
        assert_ne!(p2, w.spend_pub);
    }

    #[test]
    fn another_recipient_does_not_recognise_the_deposit() {
        let s = setup();
        let alice = wallet(&s.env, 11, 22);
        let eve = wallet(&s.env, 33, 44);
        let (ephemeral_pub, stealth_pub) = derive_stealth_address(
            &s.env,
            &alice.spend_pub,
            &alice.view_pub,
            &scalar(&s.env, 9),
        );
        let eve_scan =
            scan_stealth_address(&s.env, &eve.spend_pub, &eve.view_secret, &ephemeral_pub);
        assert_ne!(eve_scan, stealth_pub);
    }

    #[test]
    fn hash_to_scalar_is_deterministic_and_domain_separated() {
        let s = setup();
        let data = Bytes::from_slice(&s.env, b"payload");
        let a = hash_to_scalar(&s.env, TWEAK_DOMAIN, &data);
        assert_eq!(a, hash_to_scalar(&s.env, TWEAK_DOMAIN, &data));
        assert_ne!(a, hash_to_scalar(&s.env, CLAIM_DOMAIN, &data));
        assert_ne!(
            a,
            hash_to_scalar(&s.env, TWEAK_DOMAIN, &Bytes::from_slice(&s.env, b"payloae"))
        );
    }

    // ── registry ────────────────────────────────────────────────────────────

    #[test]
    fn keys_register_once_and_are_readable() {
        let s = setup();
        let w = wallet(&s.env, 11, 22);
        s.client
            .register_stealth_keys(&s.merchant, &w.spend_pub, &w.view_pub);
        let stored = s.client.get_stealth_keys(&s.merchant).unwrap();
        assert_eq!(stored.spend_pub, w.spend_pub);
        assert_eq!(stored.view_pub, w.view_pub);
        assert_eq!(
            s.client
                .try_register_stealth_keys(&s.merchant, &w.spend_pub, &w.view_pub),
            Err(Ok(StealthError::KeysAlreadyRegistered))
        );
        assert!(s.client.get_stealth_keys(&s.recipient).is_none());
    }

    #[test]
    fn invalid_keys_are_rejected() {
        let s = setup();
        let w = wallet(&s.env, 11, 22);
        let zero = BytesN::from_array(&s.env, &[0u8; 64]);
        // (1, 1) is not on y^2 = x^3 + 3.
        let mut off_curve = [0u8; 64];
        off_curve[31] = 1;
        off_curve[63] = 1;
        let off_curve = BytesN::from_array(&s.env, &off_curve);

        for bad in [zero, off_curve] {
            assert_eq!(
                s.client
                    .try_register_stealth_keys(&s.merchant, &bad, &w.view_pub),
                Err(Ok(StealthError::InvalidPoint))
            );
            assert_eq!(
                s.client
                    .try_register_stealth_keys(&s.merchant, &w.spend_pub, &bad),
                Err(Ok(StealthError::InvalidPoint))
            );
        }
        assert!(s.client.get_stealth_keys(&s.merchant).is_none());
    }

    // ── deposits ────────────────────────────────────────────────────────────

    #[test]
    fn a_deposit_moves_tokens_into_custody_and_announces_the_ephemeral_key() {
        use soroban_sdk::testutils::Events as _;
        let s = setup();
        let w = wallet(&s.env, 11, 22);
        let (commitment, ephemeral_pub, stealth_pub) = fund(&s, &w, 5, 400);

        // Events describe the last invocation, so read them first: one event
        // from the registry, carrying R for scanners.
        let events = s.env.events().all().filter_by_contract(&s.client.address);
        assert_eq!(events.events().len(), 1);

        assert_eq!(s.balance(&s.buyer), 10_000 - 400);
        assert_eq!(s.balance(&s.client.address), 400);
        let deposit = s.client.get_stealth_deposit(&commitment).unwrap();
        assert_eq!(deposit.stealth_pub, stealth_pub);
        assert_eq!(deposit.ephemeral_pub, ephemeral_pub);
        assert_eq!(deposit.amount, 400);
        assert_eq!(
            commitment,
            deposit_commitment(&s.env, &stealth_pub, &ephemeral_pub)
        );
    }

    #[test]
    fn invalid_deposits_are_rejected_without_moving_tokens() {
        let s = setup();
        let w = wallet(&s.env, 11, 22);
        let (ephemeral_pub, stealth_pub) =
            derive_stealth_address(&s.env, &w.spend_pub, &w.view_pub, &scalar(&s.env, 5));
        let zero = BytesN::from_array(&s.env, &[0u8; 64]);

        for amount in [0i128, -5] {
            assert_eq!(
                s.client.try_stealth_deposit(
                    &s.buyer,
                    &s.token,
                    &amount,
                    &ephemeral_pub,
                    &stealth_pub
                ),
                Err(Ok(StealthError::InvalidAmount))
            );
        }
        assert_eq!(
            s.client
                .try_stealth_deposit(&s.buyer, &s.token, &10i128, &zero, &stealth_pub),
            Err(Ok(StealthError::InvalidPoint))
        );
        assert_eq!(
            s.client
                .try_stealth_deposit(&s.buyer, &s.token, &10i128, &ephemeral_pub, &zero),
            Err(Ok(StealthError::InvalidPoint))
        );
        assert_eq!(s.balance(&s.buyer), 10_000);

        // The same pair twice would give two deposits one handle.
        s.client
            .stealth_deposit(&s.buyer, &s.token, &10i128, &ephemeral_pub, &stealth_pub);
        assert_eq!(
            s.client
                .try_stealth_deposit(&s.buyer, &s.token, &10i128, &ephemeral_pub, &stealth_pub),
            Err(Ok(StealthError::DuplicateDeposit))
        );
        assert_eq!(s.balance(&s.buyer), 10_000 - 10);
    }

    // ── spend authorization ─────────────────────────────────────────────────

    #[test]
    fn the_recipient_scans_finds_the_deposit_and_spends_it() {
        let s = setup();
        let w = wallet(&s.env, 11, 22);
        s.client
            .register_stealth_keys(&s.merchant, &w.spend_pub, &w.view_pub);

        // The buyer reads the registry and deposits.
        let keys = s.client.get_stealth_keys(&s.merchant).unwrap();
        let (ephemeral_pub, stealth_pub) = derive_stealth_address(
            &s.env,
            &keys.spend_pub,
            &keys.view_pub,
            &scalar(&s.env, 31337),
        );
        let commitment =
            s.client
                .stealth_deposit(&s.buyer, &s.token, &750i128, &ephemeral_pub, &stealth_pub);

        // The merchant recognises it from R alone, then derives p.
        let scanned = scan_stealth_address(&s.env, &w.spend_pub, &w.view_secret, &ephemeral_pub);
        assert_eq!(scanned, stealth_pub);
        let p = stealth_private_key(&s.env, &w.spend_secret, &w.view_secret, &ephemeral_pub);

        let (nonce_point, response) = prove(&s, &p, &stealth_pub, &commitment, &s.recipient, 4242);
        s.client
            .stealth_claim(&commitment, &s.recipient, &nonce_point, &response);

        assert_eq!(s.balance(&s.recipient), 750);
        assert_eq!(s.balance(&s.client.address), 0);
        assert!(s.client.get_stealth_deposit(&commitment).is_none());
    }

    #[test]
    fn a_wrong_key_cannot_spend() {
        let s = setup();
        let w = wallet(&s.env, 11, 22);
        let (commitment, ephemeral_pub, stealth_pub) = fund(&s, &w, 5, 400);

        // The spend secret alone, without the tweak, is not the stealth key.
        let (nonce_point, response) = prove(
            &s,
            &w.spend_secret,
            &stealth_pub,
            &commitment,
            &s.recipient,
            7,
        );
        assert_eq!(
            s.client
                .try_stealth_claim(&commitment, &s.recipient, &nonce_point, &response),
            Err(Ok(StealthError::InvalidSignature))
        );
        // An unrelated secret does not work either.
        let eve = scalar(&s.env, 99);
        let (nonce_point, response) = prove(&s, &eve, &stealth_pub, &commitment, &s.recipient, 8);
        assert_eq!(
            s.client
                .try_stealth_claim(&commitment, &s.recipient, &nonce_point, &response),
            Err(Ok(StealthError::InvalidSignature))
        );
        assert_eq!(s.balance(&s.client.address), 400, "funds untouched");
        let _ = ephemeral_pub;
    }

    #[test]
    fn a_proof_cannot_be_redirected_to_another_account() {
        let s = setup();
        let w = wallet(&s.env, 11, 22);
        let (commitment, ephemeral_pub, stealth_pub) = fund(&s, &w, 5, 400);
        let p = stealth_private_key(&s.env, &w.spend_secret, &w.view_secret, &ephemeral_pub);

        let (nonce_point, response) = prove(&s, &p, &stealth_pub, &commitment, &s.recipient, 55);
        // An observer replays the proof, naming their own account.
        let thief = Address::generate(&s.env);
        assert_eq!(
            s.client
                .try_stealth_claim(&commitment, &thief, &nonce_point, &response),
            Err(Ok(StealthError::InvalidSignature))
        );
        assert_eq!(s.balance(&thief), 0);

        // The intended recipient still can.
        s.client
            .stealth_claim(&commitment, &s.recipient, &nonce_point, &response);
        assert_eq!(s.balance(&s.recipient), 400);
    }

    #[test]
    fn a_deposit_is_spent_once() {
        let s = setup();
        let w = wallet(&s.env, 11, 22);
        let (commitment, ephemeral_pub, stealth_pub) = fund(&s, &w, 5, 400);
        let p = stealth_private_key(&s.env, &w.spend_secret, &w.view_secret, &ephemeral_pub);
        let (nonce_point, response) = prove(&s, &p, &stealth_pub, &commitment, &s.recipient, 55);

        s.client
            .stealth_claim(&commitment, &s.recipient, &nonce_point, &response);
        assert_eq!(
            s.client
                .try_stealth_claim(&commitment, &s.recipient, &nonce_point, &response),
            Err(Ok(StealthError::DepositNotFound))
        );
        assert_eq!(s.balance(&s.recipient), 400);
    }

    #[test]
    fn a_proof_for_one_deposit_does_not_open_another() {
        let s = setup();
        let w = wallet(&s.env, 11, 22);
        let (first, first_r, first_p) = fund(&s, &w, 5, 100);
        let (second, _second_r, _second_p) = fund(&s, &w, 6, 200);
        let p1 = stealth_private_key(&s.env, &w.spend_secret, &w.view_secret, &first_r);

        let (nonce_point, response) = prove(&s, &p1, &first_p, &first, &s.recipient, 3);
        assert_eq!(
            s.client
                .try_stealth_claim(&second, &s.recipient, &nonce_point, &response),
            Err(Ok(StealthError::InvalidSignature))
        );
        s.client
            .stealth_claim(&first, &s.recipient, &nonce_point, &response);
        assert_eq!(s.balance(&s.recipient), 100);
        assert!(s.client.get_stealth_deposit(&second).is_some());
    }

    #[test]
    fn a_proof_is_bound_to_the_deposit_and_not_only_to_the_stealth_key() {
        let s = setup();
        let w = wallet(&s.env, 11, 22);
        // Two deposits that share one stealth key but announce different
        // ephemeral keys, so they have different commitments.
        let (first_r, stealth_pub) =
            derive_stealth_address(&s.env, &w.spend_pub, &w.view_pub, &scalar(&s.env, 5));
        let second_r = public(&s.env, &scalar(&s.env, 6));
        let first = s
            .client
            .stealth_deposit(&s.buyer, &s.token, &100i128, &first_r, &stealth_pub);
        let second =
            s.client
                .stealth_deposit(&s.buyer, &s.token, &200i128, &second_r, &stealth_pub);
        assert_ne!(first, second);

        let p = stealth_private_key(&s.env, &w.spend_secret, &w.view_secret, &first_r);
        let (nonce_point, response) = prove(&s, &p, &stealth_pub, &first, &s.recipient, 3);
        assert_eq!(
            s.client
                .try_stealth_claim(&second, &s.recipient, &nonce_point, &response),
            Err(Ok(StealthError::InvalidSignature))
        );
        s.client
            .stealth_claim(&first, &s.recipient, &nonce_point, &response);
        assert_eq!(s.balance(&s.recipient), 100);
        assert!(s.client.get_stealth_deposit(&second).is_some());
    }

    #[test]
    fn claims_of_unknown_deposits_and_bad_nonce_points_are_rejected() {
        let s = setup();
        let w = wallet(&s.env, 11, 22);
        let (commitment, _r, _p) = fund(&s, &w, 5, 400);
        let unknown = BytesN::from_array(&s.env, &[7u8; 32]);
        let zero = BytesN::from_array(&s.env, &[0u8; 64]);
        let response = BytesN::from_array(&s.env, &[1u8; 32]);

        assert_eq!(
            s.client
                .try_stealth_claim(&unknown, &s.recipient, &zero, &response),
            Err(Ok(StealthError::DepositNotFound))
        );
        assert_eq!(
            s.client
                .try_stealth_claim(&commitment, &s.recipient, &zero, &response),
            Err(Ok(StealthError::InvalidPoint))
        );
        assert_eq!(s.balance(&s.client.address), 400);
    }
}
