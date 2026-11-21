use multisig_account::{weights, Error, MultisigAccount, MultisigAccountClient};
use soroban_sdk::{testutils::Address as _, vec, Address, Env};

#[test]
fn tally_weight_deduplicates_duplicate_signer_addresses() {
    let env = Env::default();
    env.mock_all_auths();

    let a = Address::generate(&env);
    let b = Address::generate(&env);

    // Initial threshold 1
    let id = env.register(MultisigAccount, (vec![&env, a.clone(), b.clone()], 1u32));
    let client = MultisigAccountClient::new(&env, &id);

    // Set weights: A = 10, B = 20
    client.set_signer_weight(&a, &10);
    client.set_signer_weight(&b, &20);

    // Directly test tally_weight in contract context:
    // With [A, A, B], weight must be 10 + 20 = 30, not 40.
    let approvers = vec![&env, a.clone(), a.clone(), b.clone()];
    let total = env
        .as_contract(&id, || weights::tally_weight(&env, &approvers))
        .unwrap();
    assert_eq!(total, 30);

    // With [A, B, A, B], weight must still be 30, not 60.
    let multi_dup = vec![&env, a.clone(), b.clone(), a.clone(), b.clone()];
    let total_multi = env
        .as_contract(&id, || weights::tally_weight(&env, &multi_dup))
        .unwrap();
    assert_eq!(total_multi, 30);

    // With unknown signer, returns UnknownSigner error
    let unknown = Address::generate(&env);
    let approvers_with_unknown = vec![&env, a.clone(), a.clone(), unknown];
    let err = env.as_contract(&id, || weights::tally_weight(&env, &approvers_with_unknown));
    assert_eq!(err, Err(Error::UnknownSigner));
}

#[test]
fn threshold_verification_rejects_duplicated_weights() {
    let env = Env::default();
    env.mock_all_auths();

    let a = Address::generate(&env);
    let b = Address::generate(&env);

    let id = env.register(MultisigAccount, (vec![&env, a.clone(), b.clone()], 1u32));
    let client = MultisigAccountClient::new(&env, &id);

    // A has weight 10, B has weight 20. Total weight = 30.
    client.set_signer_weight(&a, &10);
    client.set_signer_weight(&b, &20);

    // If an attacker tries to reach threshold 35 with approvers [A, A, B]
    // (which would be 40 if duplicated A was counted twice):
    let approvers = vec![&env, a.clone(), a.clone(), b.clone()];
    let weight = env
        .as_contract(&id, || weights::tally_weight(&env, &approvers))
        .unwrap();

    let target_threshold = 35u32;
    // Tally is 30, which fails the threshold check of 35:
    assert!(weight < target_threshold);
    assert_eq!(weight, 30);

    // If threshold is 30, it exactly meets it:
    let threshold_30 = 30u32;
    assert!(weight >= threshold_30);
}
