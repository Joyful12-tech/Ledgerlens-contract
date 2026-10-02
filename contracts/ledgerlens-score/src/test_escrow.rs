//! Tests for the escrow hold window (a.k.a. the finality buffer).
//!
//! A `submit_score` issued while the hold window is non-zero is parked in
//! escrow instead of being written to live storage. It becomes visible only
//! after `commit_after` elapses, and an admin may cancel it beforehand.
//!
//! ## Invariants under test
//!
//! 1. **Escrow is not live storage.** A parked score must not be readable via
//!    `get_score` and must not satisfy `query_risk_gate` — a consumer must not
//!    be able to route on a score that has not cleared its hold window.
//! 2. **Commit is gated purely on time.** `commit_after <= now` is the only
//!    condition; the committer need not be the service or the admin.
//! 3. **Cancel is admin-only and terminal.** A cancelled entry can never be
//!    committed afterwards, even once the window elapses.
//! 4. **Input validation is identical on both paths.** An out-of-range score
//!    is rejected whether the hold window is enabled or disabled — a disabled
//!    buffer must not become a way around score-range checks.
//! 5. **Commit is idempotent-by-absence.** A score commits exactly once; a
//!    second commit finds nothing pending.
//!
//! ## API note
//!
//! `get_escrow_hold_window` / `get_escrow_score` / `cancel_escrow_score` are
//! escrow-named aliases over `get_finality_buffer` / `get_pending_score` /
//! `cancel_pending_score`. This suite exercises the escrow-named surface, and
//! [`test_escrow_aliases_match_canonical_names`] pins the two together so the
//! aliases cannot silently drift.

use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger as _},
    Address, Env, Symbol, Vec,
};

use crate::{
    constants::MAX_ESCROW_HOLD_WINDOW_SECS, Error, LedgerLensScoreContract,
    LedgerLensScoreContractClient,
};

const START_TS: u64 = 1_700_000_000;

/// Hold window used by most tests. Comfortably longer than the default 1-hour
/// submission cooldown is *not* required — the cooldown is 1 hour and the
/// window is 300s, so a re-submission inside the window must still be
/// rate-limited. Tests that need both timers to pass advance by 3_601s.
const HOLD_WINDOW: u64 = 300;

fn setup<'a>() -> (Env, LedgerLensScoreContractClient<'a>, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = START_TS);

    let contract_id = env.register_contract(None, LedgerLensScoreContract);
    let client = LedgerLensScoreContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let service = Address::generate(&env);
    client.initialize(&admin, &service);

    (env, client, admin, service)
}

/// Submits a score from the service account, at the current ledger timestamp.
fn submit(
    env: &Env,
    client: &LedgerLensScoreContractClient,
    wallet: &Address,
    pair: &Symbol,
    score: u32,
) {
    client.submit_score(
        &Vec::new(env),
        wallet,
        pair,
        &score,
        &false,
        &false,
        &env.ledger().timestamp(),
        &80u32,
        &1u32,
        &None,
    );
}

fn advance_to(env: &Env, ts: u64) {
    env.ledger().with_mut(|l| l.timestamp = ts);
}

/// Builds a Soroban `Vec<Address>` from a slice.
fn signers_vec(env: &Env, addrs: &[Address]) -> Vec<Address> {
    let mut v = Vec::new(env);
    for a in addrs {
        v.push_back(a.clone());
    }
    v
}

/// True when nothing is readable on the live path.
fn no_live_score(client: &LedgerLensScoreContractClient, wallet: &Address, pair: &Symbol) -> bool {
    client.try_get_score(wallet, pair).is_err()
}

// ── Configuration ─────────────────────────────────────────────────────────────

#[test]
fn test_escrow_hold_window_default_zero() {
    let (_env, client, _admin, _service) = setup();
    assert_eq!(client.get_escrow_hold_window(), 0);
}

#[test]
fn test_set_and_get_escrow_hold_window() {
    let (env, client, _admin, _service) = setup();
    client.set_escrow_hold_window(&Vec::new(&env), &HOLD_WINDOW);
    assert_eq!(client.get_escrow_hold_window(), HOLD_WINDOW);
}

#[test]
fn test_escrow_aliases_match_canonical_names() {
    // The escrow-named accessors must stay exact aliases of the canonical
    // finality-buffer names. If either side is renamed, this fails.
    let (env, client, _admin, _service) = setup();
    client.set_escrow_hold_window(&Vec::new(&env), &HOLD_WINDOW);
    assert_eq!(client.get_escrow_hold_window(), client.get_finality_buffer());

    let wallet = Address::generate(&env);
    let pair = symbol_short!("XLM_USDC");
    submit(&env, &client, &wallet, &pair, 44);
    assert_eq!(client.get_escrow_score(&wallet, &pair), client.get_pending_score(&wallet, &pair));

    client.cancel_escrow_score(&Vec::new(&env), &wallet, &pair);
    assert_eq!(client.get_pending_score(&wallet, &pair), None);
}

#[test]
fn test_set_escrow_hold_window_above_max_rejected() {
    let (env, client, _admin, _service) = setup();
    let result =
        client.try_set_escrow_hold_window(&Vec::new(&env), &(MAX_ESCROW_HOLD_WINDOW_SECS + 1));
    assert_eq!(result, Err(Ok(Error::InvalidFinalityBuffer)));
}

#[test]
fn test_set_escrow_hold_window_at_max_accepted() {
    // Boundary: the documented maximum is inclusive.
    let (env, client, _admin, _service) = setup();
    client.set_escrow_hold_window(&Vec::new(&env), &MAX_ESCROW_HOLD_WINDOW_SECS);
    assert_eq!(client.get_escrow_hold_window(), MAX_ESCROW_HOLD_WINDOW_SECS);
}

#[test]
fn test_set_escrow_hold_window_zero_disables() {
    let (env, client, _admin, _service) = setup();
    client.set_escrow_hold_window(&Vec::new(&env), &300u64);
    client.set_escrow_hold_window(&Vec::new(&env), &0u64);
    assert_eq!(client.get_escrow_hold_window(), 0);
}

// ── Escrow path active ────────────────────────────────────────────────────────

#[test]
fn test_submit_with_escrow_writes_escrow_not_live() {
    let (env, client, _admin, _service) = setup();
    client.set_escrow_hold_window(&Vec::new(&env), &HOLD_WINDOW);

    let wallet = Address::generate(&env);
    let pair = symbol_short!("XLM_USDC");
    submit(&env, &client, &wallet, &pair, 75);

    // Not yet visible on the live path, and the risk gate fails closed.
    assert!(no_live_score(&client, &wallet, &pair));
    assert!(!client.query_risk_gate(&wallet, &pair, &100u32));
    assert_eq!(client.get_score_count(&wallet, &pair), 0);

    // But the escrow entry exists and carries the correct window.
    let entry = client.get_escrow_score(&wallet, &pair).unwrap();
    assert_eq!(entry.score, 75);
    assert_eq!(entry.submitted_at, START_TS);
    assert_eq!(entry.commit_after, START_TS + HOLD_WINDOW);
}

#[test]
fn test_escrow_entry_is_removed_after_commit() {
    let (env, client, _admin, _service) = setup();
    client.set_escrow_hold_window(&Vec::new(&env), &HOLD_WINDOW);

    let wallet = Address::generate(&env);
    let pair = symbol_short!("XLM_USDC");
    submit(&env, &client, &wallet, &pair, 75);
    assert!(client.get_escrow_score(&wallet, &pair).is_some());

    advance_to(&env, START_TS + HOLD_WINDOW);
    client.auto_commit_score(&wallet, &pair);

    // The pending entry must not linger after it becomes live, otherwise a
    // later window could re-commit stale data.
    assert!(client.get_escrow_score(&wallet, &pair).is_none());
    assert_eq!(client.get_score(&wallet, &pair).score, 75);
}

#[test]
fn test_score_becomes_visible_to_risk_gate_only_after_commit() {
    // The integration-relevant boundary: a consumer gating on this wallet must
    // see "unsafe" while the score is in escrow, and "safe" once committed.
    let (env, client, _admin, _service) = setup();
    client.set_escrow_hold_window(&Vec::new(&env), &HOLD_WINDOW);

    let wallet = Address::generate(&env);
    let pair = symbol_short!("XLM_USDC");
    submit(&env, &client, &wallet, &pair, 20); // low risk, would pass a gate of 75

    // In escrow: fails closed.
    assert!(!client.query_risk_gate(&wallet, &pair, &75u32));

    // One second early: still fails closed.
    advance_to(&env, START_TS + HOLD_WINDOW - 1);
    assert!(!client.query_risk_gate(&wallet, &pair, &75u32));

    // Committed: now the gate reflects the real score.
    advance_to(&env, START_TS + HOLD_WINDOW);
    client.auto_commit_score(&wallet, &pair);
    assert!(client.query_risk_gate(&wallet, &pair, &75u32));
}

// ── auto_commit_score eligibility ─────────────────────────────────────────────

#[test]
fn test_auto_commit_before_window_fails() {
    let (env, client, _admin, _service) = setup();
    client.set_escrow_hold_window(&Vec::new(&env), &HOLD_WINDOW);

    let wallet = Address::generate(&env);
    let pair = symbol_short!("XLM_USDC");
    submit(&env, &client, &wallet, &pair, 60);

    advance_to(&env, START_TS + HOLD_WINDOW - 1);
    let result = client.try_auto_commit_score(&wallet, &pair);
    assert_eq!(result, Err(Ok(Error::FinalityWindowNotElapsed)));
    assert!(no_live_score(&client, &wallet, &pair));
    // A rejected commit must leave the entry parked, not destroy it.
    assert!(client.get_escrow_score(&wallet, &pair).is_some());
}

#[test]
fn test_auto_commit_at_exact_window_succeeds() {
    // Boundary: `commit_after == now` is eligible (`now < commit_after` rejects).
    let (env, client, _admin, _service) = setup();
    client.set_escrow_hold_window(&Vec::new(&env), &HOLD_WINDOW);

    let wallet = Address::generate(&env);
    let pair = symbol_short!("XLM_USDC");
    submit(&env, &client, &wallet, &pair, 60);

    advance_to(&env, START_TS + HOLD_WINDOW);
    client.auto_commit_score(&wallet, &pair);

    let score = client.get_score(&wallet, &pair);
    assert_eq!(score.score, 60);
    assert!(client.get_escrow_score(&wallet, &pair).is_none());
    assert_eq!(client.get_score_count(&wallet, &pair), 1);
    assert_eq!(client.get_score_history(&wallet, &pair).len(), 1);
}

#[test]
fn test_auto_commit_after_window_also_succeeds() {
    let (env, client, _admin, _service) = setup();
    client.set_escrow_hold_window(&Vec::new(&env), &HOLD_WINDOW);

    let wallet = Address::generate(&env);
    let pair = symbol_short!("XLM_USDC");
    submit(&env, &client, &wallet, &pair, 80);

    advance_to(&env, START_TS + 1_000);
    client.auto_commit_score(&wallet, &pair);
    assert_eq!(client.get_score(&wallet, &pair).score, 80);
}

#[test]
fn test_auto_commit_is_permissionless() {
    let (env, client, _admin, _service) = setup();
    client.set_escrow_hold_window(&Vec::new(&env), &60u64);

    let wallet = Address::generate(&env);
    let pair = symbol_short!("XLM_USDC");
    submit(&env, &client, &wallet, &pair, 30);

    advance_to(&env, START_TS + 60);
    // Any address may trigger the commit — no admin or service auth involved.
    let _anyone = Address::generate(&env);
    client.auto_commit_score(&wallet, &pair);
    assert_eq!(client.get_score(&wallet, &pair).score, 30);
}

#[test]
fn test_auto_commit_no_escrow_fails() {
    let (_env, client, _wallet, _pair) = setup();
    let wallet = Address::generate(&_env);
    let pair = symbol_short!("XLM_USDC");
    let result = client.try_auto_commit_score(&wallet, &pair);
    assert_eq!(result, Err(Ok(Error::NoPendingScore)));
}

#[test]
fn test_auto_commit_is_idempotent() {
    // Duplicate commit: the second call must find nothing pending rather than
    // double-counting history or bumping the score count.
    let (env, client, _admin, _service) = setup();
    client.set_escrow_hold_window(&Vec::new(&env), &HOLD_WINDOW);

    let wallet = Address::generate(&env);
    let pair = symbol_short!("XLM_USDC");
    submit(&env, &client, &wallet, &pair, 60);

    advance_to(&env, START_TS + HOLD_WINDOW);
    client.auto_commit_score(&wallet, &pair);

    let second = client.try_auto_commit_score(&wallet, &pair);
    assert_eq!(second, Err(Ok(Error::NoPendingScore)));

    assert_eq!(client.get_score_count(&wallet, &pair), 1);
    assert_eq!(client.get_score_history(&wallet, &pair).len(), 1);
}

#[test]
fn test_auto_commit_after_cancel_fails() {
    // Cancel is terminal: once the entry is gone, waiting out the window must
    // not resurrect it.
    let (env, client, _admin, _service) = setup();
    client.set_escrow_hold_window(&Vec::new(&env), &HOLD_WINDOW);

    let wallet = Address::generate(&env);
    let pair = symbol_short!("XLM_USDC");
    submit(&env, &client, &wallet, &pair, 90);

    client.cancel_escrow_score(&Vec::new(&env), &wallet, &pair);

    advance_to(&env, START_TS + HOLD_WINDOW);
    let result = client.try_auto_commit_score(&wallet, &pair);
    assert_eq!(result, Err(Ok(Error::NoPendingScore)));
    assert!(no_live_score(&client, &wallet, &pair));
}

// ── Admin cancel ──────────────────────────────────────────────────────────────

#[test]
fn test_admin_cancel_during_window_removes_escrow() {
    let (env, client, _admin, _service) = setup();
    client.set_escrow_hold_window(&Vec::new(&env), &HOLD_WINDOW);

    let wallet = Address::generate(&env);
    let pair = symbol_short!("XLM_USDC");
    submit(&env, &client, &wallet, &pair, 90);

    advance_to(&env, START_TS + 100); // inside window
    client.cancel_escrow_score(&Vec::new(&env), &wallet, &pair);

    assert!(client.get_escrow_score(&wallet, &pair).is_none());

    // Even after the window elapses there is nothing to commit.
    advance_to(&env, START_TS + HOLD_WINDOW);
    let result = client.try_auto_commit_score(&wallet, &pair);
    assert_eq!(result, Err(Ok(Error::NoPendingScore)));
    assert!(no_live_score(&client, &wallet, &pair));
}

#[test]
fn test_admin_cancel_no_escrow_fails() {
    let (env, client, _admin, _service) = setup();
    let wallet = Address::generate(&env);
    let pair = symbol_short!("XLM_USDC");
    let result = client.try_cancel_escrow_score(&Vec::new(&env), &wallet, &pair);
    assert_eq!(result, Err(Ok(Error::NoPendingScore)));
}

#[test]
fn test_admin_cancel_only_affects_target_pair() {
    // Cancelling one wallet/pair must not disturb another wallet's escrow.
    let (env, client, _admin, _service) = setup();
    client.set_escrow_hold_window(&Vec::new(&env), &HOLD_WINDOW);

    let wallet_a = Address::generate(&env);
    let wallet_b = Address::generate(&env);
    let pair_a = symbol_short!("XLM_USDC");
    let pair_b = symbol_short!("XLM_BTC");

    submit(&env, &client, &wallet_a, &pair_a, 90);
    submit(&env, &client, &wallet_b, &pair_a, 10);
    submit(&env, &client, &wallet_a, &pair_b, 55);

    client.cancel_escrow_score(&Vec::new(&env), &wallet_a, &pair_a);

    assert!(client.get_escrow_score(&wallet_a, &pair_a).is_none());
    assert_eq!(client.get_escrow_score(&wallet_b, &pair_a).unwrap().score, 10);
    assert_eq!(client.get_escrow_score(&wallet_a, &pair_b).unwrap().score, 55);
}

#[test]
fn test_cancel_requires_admin_auth() {
    // Authorization boundary: cancelling with a non-admin signer set must be
    // rejected when an M-of-N admin set is configured.
    let (env, client, admin, _service) = setup();

    let admin_b = Address::generate(&env);
    let admin_c = Address::generate(&env);

    // Configure the hold window while still in legacy single-admin mode,
    // since every admin call requires the quorum once the set is installed.
    client.set_escrow_hold_window(&Vec::new(&env), &HOLD_WINDOW);

    // Set up a 2-of-3 admin set.
    for signer in [&admin, &admin_b, &admin_c] {
        client.add_admin_signer(&Vec::new(&env), signer);
    }
    client.set_admin_threshold(&Vec::new(&env), &2u32);

    let wallet = Address::generate(&env);
    let pair = symbol_short!("XLM_USDC");
    submit(&env, &client, &wallet, &pair, 90);

    // One valid signer where two are required.
    let too_few = signers_vec(&env, core::slice::from_ref(&admin));
    let result = client.try_cancel_escrow_score(&too_few, &wallet, &pair);
    assert_eq!(result, Err(Ok(Error::InsufficientAdminSigners)));

    // A signer that is not in the admin set.
    let outsider = Address::generate(&env);
    let not_in_set = signers_vec(&env, &[admin.clone(), outsider]);
    let result = client.try_cancel_escrow_score(&not_in_set, &wallet, &pair);
    assert_eq!(result, Err(Ok(Error::AdminSignerNotInSet)));

    // The escrow entry survives both rejected attempts.
    assert!(client.get_escrow_score(&wallet, &pair).is_some());
}

// ── Re-submission during escrow window ───────────────────────────────────────

#[test]
fn test_resubmission_after_cooldown_replaces_escrow() {
    let (env, client, _admin, _service) = setup();
    client.set_escrow_hold_window(&Vec::new(&env), &HOLD_WINDOW);

    let wallet = Address::generate(&env);
    let pair = symbol_short!("XLM_USDC");
    submit(&env, &client, &wallet, &pair, 40);

    // Past the 1-hour cooldown, still inside the original 300s escrow window.
    advance_to(&env, START_TS + 3_700);
    submit(&env, &client, &wallet, &pair, 85);

    let entry = client.get_escrow_score(&wallet, &pair).unwrap();
    assert_eq!(entry.score, 85, "the later submission replaces the parked one");
    assert_eq!(entry.submitted_at, START_TS + 3_700);
    assert_eq!(entry.commit_after, START_TS + 3_700 + HOLD_WINDOW);
}

#[test]
fn test_resubmission_replaces_escrow_and_only_new_one_commits() {
    // The superseded entry must be gone: after commit the live score is the
    // second value, and history has exactly one entry (not two).
    let (env, client, _admin, _service) = setup();
    client.set_escrow_hold_window(&Vec::new(&env), &HOLD_WINDOW);

    let wallet = Address::generate(&env);
    let pair = symbol_short!("XLM_USDC");
    submit(&env, &client, &wallet, &pair, 40);
    advance_to(&env, START_TS + 3_700);
    submit(&env, &client, &wallet, &pair, 85);

    advance_to(&env, START_TS + 3_700 + HOLD_WINDOW);
    client.auto_commit_score(&wallet, &pair);

    assert_eq!(client.get_score(&wallet, &pair).score, 85);
    assert_eq!(client.get_score_count(&wallet, &pair), 1);
    assert_eq!(client.get_score_history(&wallet, &pair).len(), 1);
}

#[test]
fn test_resubmission_before_cooldown_rate_limited() {
    let (env, client, _admin, _service) = setup();
    client.set_escrow_hold_window(&Vec::new(&env), &HOLD_WINDOW);

    let wallet = Address::generate(&env);
    let pair = symbol_short!("XLM_USDC");
    submit(&env, &client, &wallet, &pair, 40);

    // Well inside the 1-hour cooldown.
    let result = client.try_submit_score(
        &Vec::new(&env),
        &wallet,
        &pair,
        &70u32,
        &false,
        &false,
        &env.ledger().timestamp(),
        &80u32,
        &1u32,
        &None,
    );
    assert_eq!(result, Err(Ok(Error::RateLimitExceeded)));

    // The parked entry is untouched by the rejected submission.
    assert_eq!(client.get_escrow_score(&wallet, &pair).unwrap().score, 40);
}

// ── Validation parity between the two paths ───────────────────────────────────

#[test]
fn test_escrow_rejects_out_of_range_score() {
    // Regression: enabling the hold window must not let a score > 100 reach
    // escrow. The immediate-commit path rejects it via validate_risk_score;
    // the escrow path must do the same.
    let (env, client, _admin, _service) = setup();
    client.set_escrow_hold_window(&Vec::new(&env), &HOLD_WINDOW);

    let wallet = Address::generate(&env);
    let pair = symbol_short!("XLM_USDC");

    let result = client.try_submit_score(
        &Vec::new(&env),
        &wallet,
        &pair,
        &101u32,
        &false,
        &false,
        &env.ledger().timestamp(),
        &80u32,
        &1u32,
        &None,
    );
    assert_eq!(result, Err(Ok(Error::InvalidScore)));
    assert!(client.get_escrow_score(&wallet, &pair).is_none());
    assert!(no_live_score(&client, &wallet, &pair));
}

#[test]
fn test_escrow_rejects_out_of_range_confidence() {
    let (env, client, _admin, _service) = setup();
    client.set_escrow_hold_window(&Vec::new(&env), &HOLD_WINDOW);

    let wallet = Address::generate(&env);
    let pair = symbol_short!("XLM_USDC");

    let result = client.try_submit_score(
        &Vec::new(&env),
        &wallet,
        &pair,
        &50u32,
        &false,
        &false,
        &env.ledger().timestamp(),
        &101u32,
        &1u32,
        &None,
    );
    assert_eq!(result, Err(Ok(Error::InvalidConfidence)));
    assert!(client.get_escrow_score(&wallet, &pair).is_none());
}

#[test]
fn test_escrow_rejects_zero_timestamp() {
    let (env, client, _admin, _service) = setup();
    client.set_escrow_hold_window(&Vec::new(&env), &HOLD_WINDOW);

    let wallet = Address::generate(&env);
    let pair = symbol_short!("XLM_USDC");

    let result = client.try_submit_score(
        &Vec::new(&env),
        &wallet,
        &pair,
        &50u32,
        &false,
        &false,
        &0u64,
        &80u32,
        &1u32,
        &None,
    );
    assert_eq!(result, Err(Ok(Error::InvalidTimestamp)));
    assert!(client.get_escrow_score(&wallet, &pair).is_none());
}

#[test]
fn test_score_at_range_boundaries_accepted_in_escrow() {
    // Boundary pair: 0 and 100 are both valid scores and must be escrowed.
    let (env, client, _admin, _service) = setup();
    client.set_escrow_hold_window(&Vec::new(&env), &HOLD_WINDOW);

    let low = Address::generate(&env);
    let high = Address::generate(&env);
    let pair = symbol_short!("XLM_USDC");

    submit(&env, &client, &low, &pair, 0);
    assert_eq!(client.get_escrow_score(&low, &pair).unwrap().score, 0);

    // Past the cooldown so the second submission is accepted.
    advance_to(&env, START_TS + 3_601);
    submit(&env, &client, &high, &pair, 100);
    assert_eq!(client.get_escrow_score(&high, &pair).unwrap().score, 100);
}

#[test]
fn test_escrow_disabled_path_rejects_out_of_range_score() {
    // Parity: the same rejection must occur with the hold window disabled.
    let (_env, client, _admin, _service) = setup();
    let env = _env;
    assert_eq!(client.get_escrow_hold_window(), 0);

    let wallet = Address::generate(&env);
    let pair = symbol_short!("XLM_USDC");

    let result = client.try_submit_score(
        &Vec::new(&env),
        &wallet,
        &pair,
        &101u32,
        &false,
        &false,
        &env.ledger().timestamp(),
        &80u32,
        &1u32,
        &None,
    );
    assert_eq!(result, Err(Ok(Error::InvalidScore)));
    assert!(no_live_score(&client, &wallet, &pair));
}

// ── Escrow disabled — direct commit path ─────────────────────────────────────

#[test]
fn test_escrow_disabled_commits_immediately() {
    let (env, client, _admin, _service) = setup();
    assert_eq!(client.get_escrow_hold_window(), 0);

    let wallet = Address::generate(&env);
    let pair = symbol_short!("XLM_USDC");
    submit(&env, &client, &wallet, &pair, 55);

    assert_eq!(client.get_score(&wallet, &pair).score, 55);
    assert!(client.get_escrow_score(&wallet, &pair).is_none());
}

#[test]
fn test_commit_with_buffer_disabled_is_noop() {
    // With no pending entry, commit fails the same way whether or not the
    // buffer is configured.
    let (env, client, _admin, _service) = setup();
    client.set_escrow_hold_window(&Vec::new(&env), &HOLD_WINDOW);

    let wallet = Address::generate(&env);
    let pair = symbol_short!("XLM_USDC");
    let result = client.try_auto_commit_score(&wallet, &pair);
    assert_eq!(result, Err(Ok(Error::NoPendingScore)));
}
