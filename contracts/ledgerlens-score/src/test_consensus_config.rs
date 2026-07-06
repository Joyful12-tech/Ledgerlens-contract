//! Tests for `set_consensus_config`.

#![cfg(test)]

use soroban_sdk::{testutils::Address as _, Address, Env};

use crate::{Error, LedgerLensScoreContract, LedgerLensScoreContractClient};

fn setup<'a>() -> (Env, LedgerLensScoreContractClient<'a>) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, LedgerLensScoreContract);
    let client = LedgerLensScoreContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let service = Address::generate(&env);
    client.initialize(&admin, &service);
    (env, client)
}

#[test]
fn test_set_consensus_config_happy_path() {
    let (_env, client) = setup();
    client.set_consensus_config(&3, &10);
    assert_eq!(client.get_consensus_config(), (3, 10));
}

#[test]
fn test_set_consensus_config_updates_both_atomically() {
    let (_env, client) = setup();
    client.set_consensus_config(&2, &5);
    assert_eq!(client.get_consensus_config(), (2, 5));
    // Override with new values — both change together.
    client.set_consensus_config(&5, &20);
    assert_eq!(client.get_consensus_config(), (5, 20));
}

#[test]
fn test_set_consensus_config_boundary_values() {
    let (_env, client) = setup();
    // k=1 and epsilon=0 are the lowest valid values.
    client.set_consensus_config(&1, &0);
    assert_eq!(client.get_consensus_config(), (1, 0));
    // epsilon=100 is the highest valid value.
    client.set_consensus_config(&1, &100);
    assert_eq!(client.get_consensus_config(), (1, 100));
}

#[test]
fn test_set_consensus_config_k_zero_rejected() {
    let (_env, client) = setup();
    let result = client.try_set_consensus_config(&0, &5);
    assert_eq!(result, Err(Ok(Error::InvalidConsensusConfig)));
}

#[test]
fn test_set_consensus_config_epsilon_over_100_rejected() {
    let (_env, client) = setup();
    let result = client.try_set_consensus_config(&2, &101);
    assert_eq!(result, Err(Ok(Error::InvalidConsensusConfig)));
}

#[test]
fn test_set_consensus_config_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, LedgerLensScoreContract);
    let client = LedgerLensScoreContractClient::new(&env, &contract_id);

    let result = client.try_set_consensus_config(&2, &5);
    assert_eq!(result, Err(Ok(Error::NotInitialized)));
}

#[test]
#[should_panic]
fn test_set_consensus_config_non_admin_rejected() {
    let env = Env::default();
    let contract_id = env.register_contract(None, LedgerLensScoreContract);
    let client = LedgerLensScoreContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let service = Address::generate(&env);

    // Initialize with mocked auths, then disable so the auth check actually fires.
    env.mock_all_auths();
    client.initialize(&admin, &service);
    env.set_auths(&[]);

    client.set_consensus_config(&2, &5);
}
