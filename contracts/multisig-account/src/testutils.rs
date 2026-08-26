//! Test helpers for exercising the [`MultisigAccount`](crate::MultisigAccount)
//! as a contract-account admin of another contract.
//!
//! Building a transaction that authorizes a call on behalf of a *contract*
//! account requires constructing the `SorobanAuthorizationEntry` by hand —
//! wallet tooling normally does this. These helpers mirror what the wallet
//! would produce: an `AddressWithDelegates` credential for the account, with
//! the approved signers attached as delegated signers.

extern crate alloc;

use soroban_sdk::{
    xdr::{
        InvokeContractArgs, Limits, ScAddress, ScSymbol, ScVal, SorobanAddressCredentials,
        SorobanAddressCredentialsWithDelegates, SorobanAuthorizationEntry,
        SorobanAuthorizedFunction, SorobanAuthorizedInvocation, SorobanCredentials,
        SorobanDelegateSignature, StringM, VecM, WriteXdr,
    },
    Address, Env, IntoVal, TryFromVal, Val,
};

use alloc::vec::Vec;

/// Sort addresses by their XDR encoding so the delegate list is canonical
/// (the host requires delegates to be strictly increasing, with no
/// duplicates).
fn sorted_sc_addresses(addrs: &[Address]) -> Vec<ScAddress> {
    let mut sc: Vec<ScAddress> = addrs.iter().map(|a| ScAddress::from(a.clone())).collect();
    sc.sort_by(|a, b| {
        a.to_xdr(Limits::none())
            .unwrap()
            .cmp(&b.to_xdr(Limits::none()).unwrap())
    });
    sc
}

/// Build a single `SorobanDelegateSignature` for an address (no nested
/// delegates, no cryptographic signature — the test host does not verify
/// signatures).
fn delegate_sig(address: ScAddress) -> SorobanDelegateSignature {
    SorobanDelegateSignature {
        address,
        signature: ScVal::Void,
        nested_delegates: VecM::default(),
    }
}

/// Build the credentials for `account` authorizing a call, with `delegates`
/// attached as the signers the account's `__check_auth` will count.
fn credentials_with_delegates(
    account: &Address,
    nonce: i64,
    delegates: &[Address],
) -> SorobanCredentials {
    let delegate_sigs: Vec<SorobanDelegateSignature> = sorted_sc_addresses(delegates)
        .into_iter()
        .map(delegate_sig)
        .collect();
    SorobanCredentials::AddressWithDelegates(SorobanAddressCredentialsWithDelegates {
        address_credentials: SorobanAddressCredentials {
            address: ScAddress::from(account.clone()),
            nonce,
            signature_expiration_ledger: 1_000_000,
            signature: ScVal::Void,
        },
        delegates: delegate_sigs.try_into().expect("delegate list too large"),
    })
}

/// Build an authorization entry authorizing `fn_name` on `target` on behalf of
/// `account`, attaching `delegates` as the signers the account should count.
///
/// The entry returned is the exact value that must be passed to
/// `env.set_auths(&[entry])` before invoking the privileged call.
pub fn make_auth_entry(
    env: &Env,
    account: &Address,
    target: &Address,
    fn_name: &str,
    args: &[Val],
    delegates: &[Address],
) -> SorobanAuthorizationEntry {
    make_auth_entry_with_nonce(env, account, target, fn_name, args, delegates, 1)
}

/// Like [`make_auth_entry`], but with an explicit credentials `nonce`.
///
/// The test host rejects reusing the same account + nonce twice within one
/// environment, so a test that authorises the same account for several calls
/// must bump the nonce between entries.
pub fn make_auth_entry_with_nonce(
    env: &Env,
    account: &Address,
    target: &Address,
    fn_name: &str,
    args: &[Val],
    delegates: &[Address],
    nonce: i64,
) -> SorobanAuthorizationEntry {
    let fn_args: VecM<ScVal> = args
        .iter()
        .map(|v| ScVal::try_from_val(env, v).unwrap())
        .collect::<Vec<_>>()
        .try_into()
        .expect("args list too large");

    SorobanAuthorizationEntry {
        credentials: credentials_with_delegates(account, nonce, delegates),
        root_invocation: SorobanAuthorizedInvocation {
            function: SorobanAuthorizedFunction::ContractFn(InvokeContractArgs {
                contract_address: ScAddress::from(target.clone()),
                function_name: ScSymbol(fn_name.parse::<StringM<32>>().unwrap()),
                args: fn_args,
            }),
            sub_invocations: VecM::default(),
        },
    }
}

/// Build the entry for a call that itself requires no arguments (e.g. `pause`).
pub fn make_auth_entry_no_args(
    env: &Env,
    account: &Address,
    target: &Address,
    fn_name: &str,
    delegates: &[Address],
) -> SorobanAuthorizationEntry {
    make_auth_entry(env, account, target, fn_name, &[], delegates)
}

/// Convert a small list of values into `Val`s for use as call arguments.
pub fn to_args(env: &Env, values: &[impl IntoVal<Env, Val>]) -> Vec<Val> {
    values.iter().map(|v| v.into_val(env)).collect()
}
