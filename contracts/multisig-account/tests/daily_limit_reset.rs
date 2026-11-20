//! Integration tests for exact 24-hour daily limit reset timing (issue #1029).

#![cfg(test)]

extern crate alloc;

use alloc::vec::Vec;
use multisig_account::limits::WINDOW_SECONDS;
use multisig_account::{MultisigAccount, MultisigAccountClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    token::{StellarAssetClient, TokenClient},
    vec,
    xdr::{
        InvokeContractArgs, Limits, ScAddress, ScSymbol, ScVal, SorobanAddressCredentials,
        SorobanAddressCredentialsWithDelegates, SorobanAuthorizationEntry,
        SorobanAuthorizedFunction, SorobanAuthorizedInvocation, SorobanCredentials,
        SorobanDelegateSignature, StringM, VecM, WriteXdr,
    },
    Address, Env, IntoVal, TryFromVal, Val,
};

const LIMIT: i128 = 1_000;

fn sorted_sc_addresses(addrs: &[Address]) -> Vec<ScAddress> {
    let mut sc: Vec<ScAddress> = addrs.iter().map(|a| ScAddress::from(a.clone())).collect();
    sc.sort_by(|a, b| {
        a.to_xdr(Limits::none())
            .unwrap()
            .cmp(&b.to_xdr(Limits::none()).unwrap())
    });
    sc
}

fn delegate_sig(address: ScAddress) -> SorobanDelegateSignature {
    SorobanDelegateSignature {
        address,
        signature: ScVal::Void,
        nested_delegates: VecM::default(),
    }
}

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

fn make_auth_entry_with_nonce(
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

struct Fx {
    env: Env,
    account: Address,
    token: Address,
    s1: Address,
    to: Address,
    nonce: core::cell::Cell<i64>,
}

impl Fx {
    fn client(&self) -> MultisigAccountClient<'_> {
        MultisigAccountClient::new(&self.env, &self.account)
    }

    fn transfer(&self, signers: &[Address], amount: i128) -> bool {
        let args: [Val; 3] = [
            self.account.clone().into_val(&self.env),
            self.to.clone().into_val(&self.env),
            amount.into_val(&self.env),
        ];
        let nonce = self.nonce.get();
        self.nonce.set(nonce + 1);
        let entry = make_auth_entry_with_nonce(
            &self.env,
            &self.account,
            &self.token,
            "transfer",
            &args,
            signers,
            nonce,
        );
        self.env.set_auths(&[entry]);
        TokenClient::new(&self.env, &self.token)
            .try_transfer(&self.account, &self.to, &amount)
            .is_ok()
    }
}

fn setup() -> Fx {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger()
        .with_mut(|l| l.timestamp = 10 * WINDOW_SECONDS + 100);

    let (s1, s2, s3) = (
        Address::generate(&env),
        Address::generate(&env),
        Address::generate(&env),
    );
    let account = env.register(
        MultisigAccount,
        (vec![&env, s1.clone(), s2.clone(), s3], 2u32),
    );
    let sac = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&account, &100_000);
    MultisigAccountClient::new(&env, &account).set_daily_limit(&token, &LIMIT);
    env.set_auths(&[]);
    let to = Address::generate(&env);

    Fx {
        env,
        account,
        token,
        s1,
        to,
        nonce: core::cell::Cell::new(1),
    }
}

#[test]
fn test_daily_limit_resets_strictly_at_window_seconds() {
    let fx = setup();

    // 1. Spend full daily limit within current window
    assert!(fx.transfer(core::slice::from_ref(&fx.s1), LIMIT));
    assert_eq!(fx.client().get_spent_today(&fx.s1, &fx.token), LIMIT);

    // Any further spend in this window is rejected
    assert!(!fx.transfer(core::slice::from_ref(&fx.s1), 1));

    // 2. Advance time to 1 second before the next window (11 * WINDOW_SECONDS - 1)
    fx.env
        .ledger()
        .with_mut(|l| l.timestamp = 11 * WINDOW_SECONDS - 1);

    // Limit has NOT reset yet; still reflects LIMIT spent and rejects new spends
    assert_eq!(fx.client().get_spent_today(&fx.s1, &fx.token), LIMIT);
    assert!(!fx.transfer(core::slice::from_ref(&fx.s1), 1));

    // 3. Advance time to exactly the first second of the next window (11 * WINDOW_SECONDS)
    fx.env
        .ledger()
        .with_mut(|l| l.timestamp = 11 * WINDOW_SECONDS);

    // Quota resets strictly at WINDOW_SECONDS boundary
    assert_eq!(fx.client().get_spent_today(&fx.s1, &fx.token), 0);

    // Full daily limit can now be spent again
    assert!(fx.transfer(core::slice::from_ref(&fx.s1), LIMIT));
    assert_eq!(fx.client().get_spent_today(&fx.s1, &fx.token), LIMIT);

    // Once again capped in the new window
    assert!(!fx.transfer(core::slice::from_ref(&fx.s1), 1));
}
