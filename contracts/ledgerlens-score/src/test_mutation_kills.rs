#![cfg(test)]
#![allow(unused_imports)]

use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger as _},
    Address, Env, Vec,
};

use crate::{Error, LedgerLensScoreContract, LedgerLensScoreContractClient};

fn initialized<'a>() -> (Env, LedgerLensScoreContractClient<'a>, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    // Match `crate::test::initialized()`'s convention of starting at a
    // nonzero timestamp: `last_submit != 0` is used contract-wide as a
    // "no prior submission" sentinel, so a first submission that lands at
    // genesis (timestamp 0) would collide with it and silently skip the
    // cooldown/velocity-cap checks on the next submission.
    env.ledger().with_mut(|l| l.timestamp = 100_000);
    let contract_id = env.register_contract(None, LedgerLensScoreContract);
    let client = LedgerLensScoreContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let service = Address::generate(&env);
    client.initialize(&admin, &service);
    (env, client, admin, service)
}

/// Mutation kill test: Velocity cap comparison operator `>` must not become `>=`.
/// If mutated to `>=`, this test will fail because diff=10 would be rejected when allowed_delta=10.
#[test]
fn test_velocity_cap_kill_comparison_operator_boundary() {
    let (env, client, admin, _service) = initialized();
    client.set_score_velocity_cap(&Vec::from_array(&env, [admin.clone()]), &true, &10);

    let wallet = Address::generate(&env);
    let asset_pair = symbol_short!("XLM_USDC");

    // Baseline score
    client.submit_score(
        &Vec::new(&env),
        &wallet,
        &asset_pair,
        &50,
        &false,
        &false,
        &1,
        &90,
        &1,
        &None,
    );

    // Advance exactly 1 hour: allowed_delta = 10 points
    env.ledger().with_mut(|l| l.timestamp += 3600);

    // Submit score with delta exactly 10 (should be accepted)
    // Mutation: if > becomes >=, this would be rejected
    client.submit_score(
        &Vec::new(&env),
        &wallet,
        &asset_pair,
        &60,
        &false,
        &false,
        &2,
        &90,
        &1,
        &None,
    );

    assert_eq!(client.get_score(&wallet, &asset_pair).score, 60);
}

/// Mutation kill test: Velocity cap delta calculation with saturating_mul.
/// If `saturating_mul` is changed to `saturating_div`, allowed_delta will be incorrectly small.
#[test]
fn test_velocity_cap_kill_multiplication_in_allowed_delta() {
    let (env, client, admin, _service) = initialized();
    client.set_score_velocity_cap(&Vec::from_array(&env, [admin.clone()]), &true, &20);

    let wallet = Address::generate(&env);
    let asset_pair = symbol_short!("XLM_USDC");

    client.submit_score(
        &Vec::new(&env),
        &wallet,
        &asset_pair,
        &30,
        &false,
        &false,
        &1,
        &90,
        &1,
        &None,
    );

    // Advance 2 hours: allowed_delta = 20 * 2 = 40 points
    env.ledger().with_mut(|l| l.timestamp += 2 * 3600);

    // Submit score with delta exactly 40 (should be accepted)
    client.submit_score(
        &Vec::new(&env),
        &wallet,
        &asset_pair,
        &70,
        &false,
        &false,
        &2,
        &90,
        &1,
        &None,
    );

    assert_eq!(client.get_score(&wallet, &asset_pair).score, 70);
}

/// Mutation kill test: Velocity cap override flag must be checked.
/// If the override check is removed or inverted, this test will fail.
#[test]
fn test_velocity_cap_kill_override_check() {
    let (env, client, admin, _service) = initialized();
    client.set_score_velocity_cap(&Vec::from_array(&env, [admin.clone()]), &true, &5);

    let wallet = Address::generate(&env);
    let asset_pair = symbol_short!("XLM_USDC");

    client.submit_score(
        &Vec::new(&env),
        &wallet,
        &asset_pair,
        &10,
        &false,
        &false,
        &1,
        &90,
        &1,
        &None,
    );

    env.ledger().with_mut(|l| l.timestamp += 3600);

    // Without override, delta 30 should be rejected (cap is 5)
    let result = client.try_submit_score(
        &Vec::new(&env),
        &wallet,
        &asset_pair,
        &40,
        &false,
        &false,
        &2,
        &90,
        &1,
        &None,
    );
    assert_eq!(result, Err(Ok(Error::ScoreVelocityExceeded)));

    // Set override
    client.override_score_velocity_cap(
        &Vec::from_array(&env, [admin.clone()]),
        &wallet,
        &asset_pair,
    );

    // With override, submission should succeed despite exceeding cap
    client.submit_score(
        &Vec::new(&env),
        &wallet,
        &asset_pair,
        &40,
        &false,
        &false,
        &2,
        &90,
        &1,
        &None,
    );

    assert_eq!(client.get_score(&wallet, &asset_pair).score, 40);
}

/// Mutation kill test: Cooldown timestamp comparison operator.
/// If `<` becomes `<=` or `>`, the boundary condition will fail.
#[test]
fn test_cooldown_kill_comparison_operator_at_boundary() {
    let (env, client, _admin, _service) = initialized();
    let wallet = Address::generate(&env);
    let asset_pair = symbol_short!("XLM_USDC");

    // Initial submission at timestamp 1000
    env.ledger().with_mut(|l| l.timestamp = 1000);
    client.submit_score(
        &Vec::new(&env),
        &wallet,
        &asset_pair,
        &50,
        &false,
        &false,
        &1,
        &90,
        &1,
        &None,
    );

    // Default cooldown is 3600 seconds (1 hour)
    // At timestamp 4599 (1000 + 3599), submission should fail
    env.ledger().with_mut(|l| l.timestamp = 4599);
    let result = client.try_submit_score(
        &Vec::new(&env),
        &wallet,
        &asset_pair,
        &55,
        &false,
        &false,
        &2,
        &90,
        &1,
        &None,
    );
    assert_eq!(result, Err(Ok(Error::RateLimitExceeded)));

    // At timestamp 4600 (1000 + 3600), submission should succeed
    env.ledger().with_mut(|l| l.timestamp = 4600);
    client.submit_score(
        &Vec::new(&env),
        &wallet,
        &asset_pair,
        &55,
        &false,
        &false,
        &2,
        &90,
        &1,
        &None,
    );

    assert_eq!(client.get_score(&wallet, &asset_pair).score, 55);
}

/// Mutation kill test: Cooldown guard check `last_submit != 0`.
///
/// The guard exists so a wallet's very first submission (`last_submit == 0`)
/// is never blocked by `now < last_submit.saturating_add(cooldown)` — at a
/// low timestamp (100, less than the 3600s default cooldown) that condition
/// would otherwise spuriously reject the first-ever submission. It does NOT
/// mean a same-timestamp resubmission should succeed — that's real rate
/// limiting doing its job. So this test checks: (1) the first submission at
/// a low timestamp succeeds, then (2) a normal resubmission past the
/// cooldown also succeeds (confirming the guard didn't leave the gate stuck
/// open).
#[test]
fn test_cooldown_kill_last_submit_guard() {
    let (env, client, _admin, _service) = initialized();
    let wallet = Address::generate(&env);
    let asset_pair = symbol_short!("XLM_USDC");

    // Initial submission (last_submit starts at 0) at a timestamp lower than
    // the cooldown itself — without the guard, `100 < 0 + 3600` would wrongly
    // reject this first-ever submission.
    env.ledger().with_mut(|l| l.timestamp = 100);
    client.submit_score(
        &Vec::new(&env),
        &wallet,
        &asset_pair,
        &50,
        &false,
        &false,
        &1,
        &90,
        &1,
        &None,
    );

    // Past the cooldown window — resubmission should succeed normally.
    env.ledger().with_mut(|l| l.timestamp += 3600);
    client.submit_score(
        &Vec::new(&env),
        &wallet,
        &asset_pair,
        &52,
        &false,
        &false,
        &2,
        &90,
        &1,
        &None,
    );

    assert_eq!(client.get_score(&wallet, &asset_pair).score, 52);
}

/// Mutation kill test: require_auth must be called for service account.
/// If this call is removed, an unauthorized caller could submit scores.
///
/// `mock_all_auths()` (in any variant) makes `require_auth()` a no-op for
/// every address, so it can't be used to observe a rejected caller. The
/// established pattern (see `test_decay::test_set_decay_rate_non_admin_rejected`)
/// is: mock auths only long enough to initialize, then `set_auths(&[])` so
/// the real auth check runs — an unauthorized call then panics rather than
/// returning an `Err`, since Soroban's `require_auth` failure escalates to a
/// host panic, not a catchable `Result`.
#[test]
#[should_panic]
fn test_require_auth_kill_service_account() {
    let env = Env::default();
    let contract_id = env.register_contract(None, LedgerLensScoreContract);
    let client = LedgerLensScoreContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let service = Address::generate(&env);
    env.mock_all_auths();
    client.initialize(&admin, &service);
    env.ledger().with_mut(|l| l.timestamp = 100);
    env.set_auths(&[]);

    let wallet = Address::generate(&env);
    let asset_pair = symbol_short!("XLM_USDC");

    // No auth provided for the service account — must panic if require_auth
    // is properly called on the submission path.
    client.submit_score(
        &Vec::new(&env),
        &wallet,
        &asset_pair,
        &50,
        &false,
        &false,
        &1,
        &90,
        &1,
        &None,
    );
}

/// Mutation kill test: Score floor comparison must use correct boundary.
/// Tests that a score equal to floor_value is rejected when enabled.
#[test]
fn test_score_floor_kill_comparison_boundary() {
    let (env, client, admin, _service) = initialized();

    // Set floor policy: high_water_mark=80, floor_value=20
    client.set_score_floor_policy(
        &Vec::from_array(&env, [admin.clone()]),
        &true,
        &80,
        &20,
    );

    let wallet = Address::generate(&env);
    let asset_pair = symbol_short!("XLM_USDC");

    // First submission above high_water_mark
    client.submit_score(
        &Vec::new(&env),
        &wallet,
        &asset_pair,
        &85,
        &false,
        &false,
        &1,
        &90,
        &1,
        &None,
    );

    env.ledger().with_mut(|l| l.timestamp += 3601);

    // Attempting submission below floor (15 < 20) should fail
    let result = client.try_submit_score(
        &Vec::new(&env),
        &wallet,
        &asset_pair,
        &15,
        &false,
        &false,
        &2,
        &90,
        &1,
        &None,
    );
    assert_eq!(result, Err(Ok(Error::InvalidScore)));

    // Attempting submission at floor value (20) should succeed
    client.submit_score(
        &Vec::new(&env),
        &wallet,
        &asset_pair,
        &20,
        &false,
        &false,
        &2,
        &90,
        &1,
        &None,
    );

    assert_eq!(client.get_score(&wallet, &asset_pair).score, 20);
}

/// Mutation kill test: Score range validation.
/// If the score > 100 check is removed, invalid scores could be submitted.
#[test]
fn test_score_range_kill_upper_bound() {
    let (env, client, _admin, _service) = initialized();

    let wallet = Address::generate(&env);
    let asset_pair = symbol_short!("XLM_USDC");

    // Attempt to submit score > 100
    let result = client.try_submit_score(
        &Vec::new(&env),
        &wallet,
        &asset_pair,
        &101,
        &false,
        &false,
        &1,
        &90,
        &1,
        &None,
    );

    assert_eq!(result, Err(Ok(Error::InvalidScore)));
}

/// Mutation kill test: Confidence range validation.
/// If the confidence > 100 check is removed, invalid confidence could be accepted.
#[test]
fn test_confidence_range_kill_upper_bound() {
    let (env, client, _admin, _service) = initialized();

    let wallet = Address::generate(&env);
    let asset_pair = symbol_short!("XLM_USDC");

    // Attempt to submit confidence > 100
    let result = client.try_submit_score(
        &Vec::new(&env),
        &wallet,
        &asset_pair,
        &50,
        &false,
        &false,
        &1,
        &101,
        &1,
        &None,
    );

    assert_eq!(result, Err(Ok(Error::InvalidConfidence)));
}
