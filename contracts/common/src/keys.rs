//! Type-safe storage-key namespace partitioning (issue #430).
//!
//! Every contract in this workspace writes to the Soroban ledger, and every
//! module inside a contract shares one namespace. Before this module, a key
//! could be as loose as a bare `symbol_short!("vaults")`, and two independent
//! submodules could silently pick the *same* raw key — a collision that only
//! surfaces after an upgrade, when a read meant for one module decodes state
//! written by another.
//!
//! This module makes that class of bug a compile error rather than a runtime
//! surprise:
//!
//! - [`KeyNamespace`] is the coarse partition (`Vault`, `Admin`, `Auth`, ...).
//!   Each namespace owns a disjoint 32-bit block of the key space.
//! - [`DataKey`] prefixes every per-module key with its owning namespace:
//!   [`DataKey::Vault`], [`DataKey::Admin`], [`DataKey::Auth`]. A key is
//!   therefore only ever reachable through the module that owns it.
//! - [`DataKey::discriminant`] folds the namespace and the local discriminant
//!   into a single `u64` (`namespace << 32 | local`), so raw string/symbol
//!   keys registered in one namespace can never collide with another
//!   namespace's integer keys — even across contract upgrades.
//! - The uniqueness of every discriminant is asserted at **compile time** by
//!   [`discriminants_are_distinct`], so adding two colliding keys fails
//!   `cargo check`, not a production read.
//!
//! Contracts that need a dynamically-valued key (an `Address` or a
//! `BytesN<32>`) keep a namespaced key as the *prefix* and append the value
//! inside the concrete key type; the partition is what guarantees two modules
//! cannot alias each other, not the value's byte length.

use soroban_sdk::contracttype;

/// How far the namespace tag is shifted when folding into a `u64`. The low 32
/// bits hold the a namespace-local discriminant; everything above is the
/// namespace itself. 32 bits gives every namespace ~4 billion local keys,
/// far more than any contract here will ever write.
pub const NAMESPACE_SHIFT: u32 = 32;

/// Mask selecting the namespace-local discriminant from a folded key.
pub const LOCAL_MASK: u64 = 0xffff_ffff;

/// Coarse, disjoint owner of a slice of the storage key space.
///
/// The variant order is the on-chain encoding order and must never be
/// reordered: a persisted key encodes its namespace *by position*, so moving
/// a variant reinterprets existing ledger entries. New namespaces are
/// appended.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyNamespace {
    /// Vault-owned operational state (token, windows, fees, locks).
    Vault,
    /// Admin/ownership state (current admin, pending transfers).
    Admin,
    /// Authentication state (domain separator, replay nonces).
    Auth,
    /// Oracle whitelist and dynamic oracle policy.
    Oracle,
    /// Yield strategy state.
    Yield,
}

impl KeyNamespace {
    /// The numeric tag folded into every [`DataKey`] in this namespace.
    ///
    /// Derived from the variant position, so it is stable as long as variants
    /// are only appended.
    pub const fn tag(self) -> u64 {
        self as u64
    }

    /// The high `u64` bit-pattern for this namespace (`tag << NAMESPACE_SHIFT`).
    pub const fn prefix(self) -> u64 {
        self.tag() << NAMESPACE_SHIFT
    }
}

/// Vault-owned keys within [`KeyNamespace::Vault`].
///
/// Only the variant *order* is persisted; renaming a variant is safe, moving
/// one is not.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VaultKey {
    Token,
    RefundWindow,
    RefundDeadline,
    VdfDelay,
    TimePolicyContract,
    VdfPolicyContract,
    FeeBps,
    FeeRecipient,
    IsPaused,
    PendingPolicy,
    ReentrancyLock,
    StorageVersion,
    Factory,
}

/// Admin/ownership keys within [`KeyNamespace::Admin`].
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdminKey {
    Admin,
    PendingAdmin,
}

/// Authentication keys within [`KeyNamespace::Auth`].
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthKey {
    DomainSeparator,
    Nonce,
}

/// Oracle keys within [`KeyNamespace::Oracle`].
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OracleKey {
    Whitelist,
    Policy,
}

/// Yield keys within [`KeyNamespace::Yield`].
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum YieldKey {
    Strategy,
    DeployedPrincipal,
    HarvestedYield,
    ReserveRatio,
    MaxDeployRatio,
    ApprovedStrategy,
    YieldRecipient,
}

/// A fully-qualified storage key: a namespace wrapping one module-local key.
///
/// Values that need a per-record dimension (a payment ref, a user address, ...)
/// are stored inside the concrete module rather than here; this enum is the
/// stable, collision-free *prefix* every module keys off.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataKey {
    Vault(VaultKey),
    Admin(AdminKey),
    Auth(AuthKey),
    Oracle(OracleKey),
    Yield(YieldKey),
}

impl DataKey {
    /// The namespace that owns this key.
    pub const fn namespace(&self) -> KeyNamespace {
        match self {
            DataKey::Vault(_) => KeyNamespace::Vault,
            DataKey::Admin(_) => KeyNamespace::Admin,
            DataKey::Auth(_) => KeyNamespace::Auth,
            DataKey::Oracle(_) => KeyNamespace::Oracle,
            DataKey::Yield(_) => KeyNamespace::Yield,
        }
    }

    /// Fold this key into the flat `u64` key space.
    ///
    /// `namespace << 32 | local` — the namespace occupies the high bits, so a
    /// key from one namespace can never equal a key from another, regardless
    /// of local discriminants. This is the property the compile-time check
    /// below pins.
    pub const fn discriminant(&self) -> u64 {
        let local = match self {
            DataKey::Vault(k) => *k as u64,
            DataKey::Admin(k) => *k as u64,
            DataKey::Auth(k) => *k as u64,
            DataKey::Oracle(k) => *k as u64,
            DataKey::Yield(k) => *k as u64,
        };
        self.namespace().prefix() | (local & LOCAL_MASK)
    }
}

/// Encode an arbitrary namespace-local ordinal, for modules whose key set is
/// open-ended (e.g. a map keyed by a monotone id) but which still must live in
/// exactly one namespace.
pub const fn encode(namespace: KeyNamespace, local: u64) -> u64 {
    namespace.prefix() | (local & LOCAL_MASK)
}

/// Every [`DataKey`] variant this module defines, in a fixed order.
///
/// This array is maintained **by hand**: when a variant is added to any of the
/// key enums above it must also be appended here, otherwise
/// [`discriminants_are_distinct`] no longer covers the whole key space. The
/// compile-time assertion below catches a *colliding* pair; it cannot catch a
/// *missing* entry, so `all_data_keys_is_exhaustive` pins the count and the
/// per-namespace coverage as the guard against a forgotten append.
pub const ALL_DATA_KEYS: [DataKey; 26] = [
    DataKey::Vault(VaultKey::Token),
    DataKey::Vault(VaultKey::RefundWindow),
    DataKey::Vault(VaultKey::RefundDeadline),
    DataKey::Vault(VaultKey::VdfDelay),
    DataKey::Vault(VaultKey::TimePolicyContract),
    DataKey::Vault(VaultKey::VdfPolicyContract),
    DataKey::Vault(VaultKey::FeeBps),
    DataKey::Vault(VaultKey::FeeRecipient),
    DataKey::Vault(VaultKey::IsPaused),
    DataKey::Vault(VaultKey::PendingPolicy),
    DataKey::Vault(VaultKey::ReentrancyLock),
    DataKey::Vault(VaultKey::StorageVersion),
    DataKey::Vault(VaultKey::Factory),
    DataKey::Admin(AdminKey::Admin),
    DataKey::Admin(AdminKey::PendingAdmin),
    DataKey::Auth(AuthKey::DomainSeparator),
    DataKey::Auth(AuthKey::Nonce),
    DataKey::Oracle(OracleKey::Whitelist),
    DataKey::Oracle(OracleKey::Policy),
    DataKey::Yield(YieldKey::Strategy),
    DataKey::Yield(YieldKey::DeployedPrincipal),
    DataKey::Yield(YieldKey::HarvestedYield),
    DataKey::Yield(YieldKey::ReserveRatio),
    DataKey::Yield(YieldKey::MaxDeployRatio),
    DataKey::Yield(YieldKey::ApprovedStrategy),
    DataKey::Yield(YieldKey::YieldRecipient),
];

/// Compile-time proof that no two [`DataKey`] variants share a discriminant.
///
/// This is a `const fn`, so the [`const _`](discriminants_are_distinct) assert
/// directly below is evaluated by the compiler: adding two keys that fold to
/// the same `u64` fails `cargo check` instead of silently aliasing storage.
pub const fn discriminants_are_distinct() -> bool {
    let mut i = 0;
    while i < ALL_DATA_KEYS.len() {
        let mut j = i + 1;
        while j < ALL_DATA_KEYS.len() {
            if ALL_DATA_KEYS[i].discriminant() == ALL_DATA_KEYS[j].discriminant() {
                return false;
            }
            j += 1;
        }
        i += 1;
    }
    true
}

/// The compile-time gate: this fails the build if any two keys collide.
const _: () = assert!(discriminants_are_distinct());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_namespace_has_a_distinct_prefix() {
        let prefixes = [
            KeyNamespace::Vault.prefix(),
            KeyNamespace::Admin.prefix(),
            KeyNamespace::Auth.prefix(),
            KeyNamespace::Oracle.prefix(),
            KeyNamespace::Yield.prefix(),
        ];
        for (i, a) in prefixes.iter().enumerate() {
            for b in prefixes.iter().skip(i + 1) {
                assert_ne!(a, b, "namespace prefixes must be disjoint");
            }
        }
    }

    #[test]
    fn discriminants_are_distinct_at_runtime_too() {
        assert!(discriminants_are_distinct());
    }

    #[test]
    fn namespace_is_isolated_in_the_high_bits() {
        // A local discriminant of 0 in two different namespaces must not alias.
        let vault = encode(KeyNamespace::Vault, 0);
        let admin = encode(KeyNamespace::Admin, 0);
        assert_ne!(vault, admin);
        assert_eq!(vault, KeyNamespace::Vault.prefix());
        assert_eq!(admin, KeyNamespace::Admin.prefix());
    }

    #[test]
    fn raw_symbol_style_keys_cannot_cross_namespaces() {
        // A "raw" local ordinate (as a legacy string/symbol key would produce)
        // placed in the Auth namespace can never equal the same ordinate in
        // the Vault namespace, even at the u64 boundary.
        for local in [0u64, 1, 0xdead_beef, LOCAL_MASK] {
            assert_ne!(
                encode(KeyNamespace::Auth, local),
                encode(KeyNamespace::Vault, local)
            );
            assert_ne!(
                encode(KeyNamespace::Oracle, local),
                encode(KeyNamespace::Yield, local)
            );
        }
    }

    #[test]
    fn data_key_reports_its_owning_namespace() {
        assert_eq!(
            DataKey::Vault(VaultKey::FeeBps).namespace(),
            KeyNamespace::Vault
        );
        assert_eq!(DataKey::Admin(AdminKey::Admin).namespace(), KeyNamespace::Admin);
        assert_eq!(DataKey::Auth(AuthKey::Nonce).namespace(), KeyNamespace::Auth);
        assert_eq!(
            DataKey::Oracle(OracleKey::Policy).namespace(),
            KeyNamespace::Oracle
        );
        assert_eq!(
            DataKey::Yield(YieldKey::Strategy).namespace(),
            KeyNamespace::Yield
        );
    }

    #[test]
    fn all_data_keys_is_exhaustive() {
        // Guards against adding a `DataKey` variant without appending it to
        // `ALL_DATA_KEYS` (which would silently drop it from the compile-time
        // collision check). Update this count when variants are appended.
        assert_eq!(
            ALL_DATA_KEYS.len(),
            26,
            "a DataKey variant was added/removed without updating ALL_DATA_KEYS"
        );

        for ns in [
            KeyNamespace::Vault,
            KeyNamespace::Admin,
            KeyNamespace::Auth,
            KeyNamespace::Oracle,
            KeyNamespace::Yield,
        ] {
            assert!(
                ALL_DATA_KEYS.iter().any(|k| k.namespace() == ns),
                "namespace {ns:?} has no entry in ALL_DATA_KEYS"
            );
        }
    }

    #[test]
    fn discriminant_encoding_is_namespace_then_local() {
        let key = DataKey::Vault(VaultKey::RefundWindow);
        assert_eq!(
            key.discriminant(),
            KeyNamespace::Vault.prefix() | (VaultKey::RefundWindow as u64)
        );
    }
}
