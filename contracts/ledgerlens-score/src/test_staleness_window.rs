//! Tests for set_staleness_window / get_staleness_window.

use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Events as _, Ledger as _},
    Address, Env, IntoVal, Vec,
};

use crate::{Error, LedgerLensScoreContract, LedgerLensScoreContractClient};

fn setup<'a>() -> (Env, LedgerLensScoreContractClient<'a>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, LedgerLensScoreContract);
    let client = LedgerLensScoreContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let service = Address::generate(&env);
    client.initialize(&admin, &service);
    (env, client, admin)
}

#[test]
fn test_set_staleness_window_happy_path() {
    let (env, client, _admin) = setup();
    let empty: Vec<Address> = Vec::new(&env);
    client.set_staleness_window(&empty, &3_600);
    // set_staleness_window is time-locked; advance past the delay and apply it.
    env.ledger().with_mut(|l| l.timestamp += 86_401);
    client.apply_param_change(&symbol_short!("stale_w"));
    assert_eq!(client.get_staleness_window(), 3_600);
}

#[test]
fn test_set_staleness_window_emits_event() {
    let (env, client, _admin) = setup();
    let contract_id = env.register_contract(None, LedgerLensScoreContract);
    let c2 = LedgerLensScoreContractClient::new(&env, &contract_id);
    c2.initialize(&Address::generate(&env), &Address::generate(&env));
    let empty: Vec<Address> = Vec::new(&env);
    c2.set_staleness_window(&empty, &7_200);

    // set_staleness_window proposes via the generic timelock, emitting
    // `param_change_proposed` (topic "pc_prop") rather than an immediate update.
    let topic = (symbol_short!("pc_prop"),);
    let found = env.events().all().iter().any(|(addr, topics, data)| {
        if addr != contract_id || topics != topic.clone().into_val(&env) {
            return false;
        }
        let (key, _apply_after): (soroban_sdk::Symbol, u64) = data.into_val(&env);
        key == symbol_short!("stale_w")
    });
    assert!(found, "param_change_proposed event not emitted for stale_w");
}

#[test]
fn test_set_staleness_window_rejects_zero() {
    let (env, client, _admin) = setup();
    let empty: Vec<Address> = Vec::new(&env);
    let result = client.try_set_staleness_window(&empty, &0);
    assert_eq!(result, Err(Ok(Error::InvalidStalenessWindow)));
}

#[test]
#[should_panic]
fn test_set_staleness_window_non_admin_rejected() {
    let env = Env::default();
    let contract_id = env.register_contract(None, LedgerLensScoreContract);
    let client = LedgerLensScoreContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let service = Address::generate(&env);
    // initialize with mocked auths, then disable so the auth check actually fires.
    env.mock_all_auths();
    client.initialize(&admin, &service);
    env.set_auths(&[]);
    let non_admin: Vec<Address> = Vec::new(&env);
    client.set_staleness_window(&non_admin, &3_600);
}
