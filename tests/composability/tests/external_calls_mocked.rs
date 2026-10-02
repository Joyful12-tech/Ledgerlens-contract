//! Cross-contract coverage for the escrow hold window (finality buffer).
//!
//! The unit suite in `contracts/ledgerlens-score/src/test_escrow.rs` proves
//! the escrow state machine in isolation. This file covers the part that only
//! shows up across a real contract boundary: **what an external integrator
//! sees while a score is parked.**
//!
//! `mock-amm` and `mock-lending` are the external callers. Both consult
//! LedgerLens through `query_risk_gate` / `query_risk_gate_with_confidence`
//! and fail closed when the gate returns `false`. A score sitting in escrow is
//! not yet live storage, so from an integrator's point of view it must be
//! indistinguishable from "no score at all" — an untrusted or a not-yet-final
//! score must never be usable to route.
//!
//! The invariant pinned here:
//!
//! > A parked score must not authorise anything. It must fail closed at every
//! > external call site, and the same calls must succeed only once the hold
//! > window has elapsed and the score has been committed to live storage.

use ledgerlens_score::{LedgerLensScoreContract, LedgerLensScoreContractClient};
use mock_amm::{MockAmm, MockAmmClient, MockAmmError};
use mock_lending::{MockLending, MockLendingClient, MockLendingError};
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger as _},
    Address, Env, Vec,
};

const GATE_THRESHOLD: u32 = 75;
const MIN_CONFIDENCE: u32 = 50;
const START_TS: u64 = 1_700_000_000;
const HOLD_WINDOW: u64 = 300;

struct Fixture<'a> {
    env: Env,
    ledgerlens: LedgerLensScoreContractClient<'a>,
    amm: MockAmmClient<'a>,
    lending: MockLendingClient<'a>,
}

/// Deploys LedgerLens plus both mocks in one shared `Env`, then enables the
/// escrow hold window.
fn setup<'a>() -> Fixture<'a> {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = START_TS);

    let ledgerlens_id = env.register_contract(None, LedgerLensScoreContract);
    let ledgerlens = LedgerLensScoreContractClient::new(&env, &ledgerlens_id);
    let admin = Address::generate(&env);
    let service = Address::generate(&env);
    ledgerlens.initialize(&admin, &service);

    // Hold window on: every submit_score is parked until committed.
    ledgerlens.set_escrow_hold_window(&Vec::new(&env), &HOLD_WINDOW);

    let amm_id = env.register_contract(None, MockAmm);
    let amm = MockAmmClient::new(&env, &amm_id);
    amm.initialize(&ledgerlens_id, &GATE_THRESHOLD);
    amm.set_liquidity_gate_config(&GATE_THRESHOLD, &MIN_CONFIDENCE);

    let lending_id = env.register_contract(None, MockLending);
    let lending = MockLendingClient::new(&env, &lending_id);
    lending.initialize(&ledgerlens_id, &GATE_THRESHOLD, &MIN_CONFIDENCE);

    Fixture { env, ledgerlens, amm, lending }
}

/// Submits a score from the service account at the current timestamp. Unlike
/// the immediate-commit path this does **not** pre-advance the ledger, so
/// tests control exactly when the hold window opens and closes.
fn submit_into_escrow(fixture: &Fixture, wallet: &Address, score: u32, confidence: u32) {
    fixture.ledgerlens.submit_score(
        &Vec::new(&fixture.env),
        wallet,
        &symbol_short!("XLM_USDC"),
        &score,
        &false,
        &false,
        &fixture.env.ledger().timestamp(),
        &confidence,
        &1u32,
        &None,
    );
}

fn advance_to(fixture: &Fixture, ts: u64) {
    fixture.env.ledger().with_mut(|l| l.timestamp = ts);
}

// ── A parked score authorises nothing ─────────────────────────────────────────

#[test]
fn amm_swap_rejected_while_score_is_in_escrow() {
    // Score 10 would comfortably pass a gate of 75, but it is not live yet.
    let fixture = setup();
    let wallet = Address::generate(&fixture.env);
    submit_into_escrow(&fixture, &wallet, 10, 90);

    assert!(fixture.ledgerlens.get_pending_score(&wallet, &symbol_short!("XLM_USDC")).is_some());

    let result = fixture.amm.try_swap(&wallet, &symbol_short!("XLM_USDC"), &1_000);
    assert_eq!(result, Err(Ok(MockAmmError::HighRiskWallet)));
}

#[test]
fn amm_liquidity_rejected_while_score_is_in_escrow() {
    let fixture = setup();
    let wallet = Address::generate(&fixture.env);
    submit_into_escrow(&fixture, &wallet, 10, 90);

    let result = fixture.amm.try_provide_liquidity_gated(&wallet, &1_000);
    assert_eq!(result, Err(Ok(MockAmmError::HighRiskWallet)));
}

#[test]
fn lending_borrow_rejected_while_score_is_in_escrow() {
    let fixture = setup();
    let wallet = Address::generate(&fixture.env);
    submit_into_escrow(&fixture, &wallet, 10, 90);

    let result = fixture.lending.try_borrow(&wallet, &symbol_short!("XLM_USDC"), &1_000);
    assert_eq!(result, Err(Ok(MockLendingError::RiskGateRejected)));
}

// ── The window boundary is what flips the decision ────────────────────────────

#[test]
fn external_calls_still_rejected_one_second_before_commit() {
    let fixture = setup();
    let wallet = Address::generate(&fixture.env);
    submit_into_escrow(&fixture, &wallet, 10, 90);

    advance_to(&fixture, START_TS + HOLD_WINDOW - 1);

    assert_eq!(
        fixture.amm.try_swap(&wallet, &symbol_short!("XLM_USDC"), &1_000),
        Err(Ok(MockAmmError::HighRiskWallet))
    );
    assert_eq!(
        fixture.lending.try_borrow(&wallet, &symbol_short!("XLM_USDC"), &1_000),
        Err(Ok(MockLendingError::RiskGateRejected))
    );
}

#[test]
fn external_calls_allowed_at_exact_commit_boundary() {
    // Boundary: `commit_after == now` is eligible, and the commit is
    // permissionless, so any third party can trigger the transition that
    // unblocks every integrator.
    let fixture = setup();
    let wallet = Address::generate(&fixture.env);
    submit_into_escrow(&fixture, &wallet, 10, 90);

    advance_to(&fixture, START_TS + HOLD_WINDOW);
    let _third_party = Address::generate(&fixture.env);
    fixture.ledgerlens.auto_commit_score(&wallet, &symbol_short!("XLM_USDC"));

    assert_eq!(fixture.amm.try_swap(&wallet, &symbol_short!("XLM_USDC"), &1_000), Ok(Ok(())));
    assert_eq!(fixture.lending.try_borrow(&wallet, &symbol_short!("XLM_USDC"), &1_000), Ok(Ok(())));
}

#[test]
fn external_calls_allowed_after_commit_without_committing_early() {
    let fixture = setup();
    let wallet = Address::generate(&fixture.env);
    submit_into_escrow(&fixture, &wallet, 10, 90);

    // Commit too early — must be refused, and must not unlock anything.
    let early = fixture.ledgerlens.try_auto_commit_score(&wallet, &symbol_short!("XLM_USDC"));
    assert!(early.is_err());
    assert_eq!(
        fixture.amm.try_swap(&wallet, &symbol_short!("XLM_USDC"), &1_000),
        Err(Ok(MockAmmError::HighRiskWallet))
    );

    // Now commit properly and the same call succeeds.
    advance_to(&fixture, START_TS + HOLD_WINDOW);
    fixture.ledgerlens.auto_commit_score(&wallet, &symbol_short!("XLM_USDC"));
    assert_eq!(fixture.amm.try_swap(&wallet, &symbol_short!("XLM_USDC"), &1_000), Ok(Ok(())));
}

#[test]
fn committed_low_risk_score_unblocks_amm_after_full_window() {
    let fixture = setup();
    let wallet = Address::generate(&fixture.env);
    submit_into_escrow(&fixture, &wallet, 10, 90);

    advance_to(&fixture, START_TS + 5_000);
    fixture.ledgerlens.auto_commit_score(&wallet, &symbol_short!("XLM_USDC"));

    assert_eq!(fixture.amm.try_swap(&wallet, &symbol_short!("XLM_USDC"), &1_000), Ok(Ok(())));
    assert_eq!(fixture.amm.try_provide_liquidity_gated(&wallet, &1_000), Ok(Ok(())));
}

#[test]
fn committed_high_risk_score_still_rejected_after_full_window() {
    // Committing must not launder a risky score: the gate keeps refusing.
    let fixture = setup();
    let wallet = Address::generate(&fixture.env);
    submit_into_escrow(&fixture, &wallet, 95, 90);

    advance_to(&fixture, START_TS + 5_000);
    fixture.ledgerlens.auto_commit_score(&wallet, &symbol_short!("XLM_USDC"));

    assert_eq!(
        fixture.amm.try_swap(&wallet, &symbol_short!("XLM_USDC"), &1_000),
        Err(Ok(MockAmmError::HighRiskWallet))
    );
    assert_eq!(
        fixture.lending.try_borrow(&wallet, &symbol_short!("XLM_USDC"), &1_000),
        Err(Ok(MockLendingError::RiskGateRejected))
    );
}

// ── Cancelled escrow never authorises anything, ever ──────────────────────────

#[test]
fn admin_cancelled_score_never_unblocks_external_calls() {
    let fixture = setup();
    let wallet = Address::generate(&fixture.env);
    submit_into_escrow(&fixture, &wallet, 10, 90);

    fixture.ledgerlens.cancel_pending_score(
        &Vec::new(&fixture.env),
        &wallet,
        &symbol_short!("XLM_USDC"),
    );

    // Well past the original window: the score must stay dead.
    advance_to(&fixture, START_TS + 10_000);

    assert!(fixture.ledgerlens.get_pending_score(&wallet, &symbol_short!("XLM_USDC")).is_none());
    assert!(fixture.ledgerlens.try_auto_commit_score(&wallet, &symbol_short!("XLM_USDC")).is_err());
    assert_eq!(
        fixture.amm.try_swap(&wallet, &symbol_short!("XLM_USDC"), &1_000),
        Err(Ok(MockAmmError::HighRiskWallet))
    );
    assert_eq!(
        fixture.lending.try_borrow(&wallet, &symbol_short!("XLM_USDC"), &1_000),
        Err(Ok(MockLendingError::RiskGateRejected))
    );
}

// ── A superseded escrow entry exposes only the surviving value ────────────────

#[test]
fn external_calls_observe_only_the_latest_escrowed_score() {
    // First submission is risky, second (after the cooldown) is benign. Once
    // committed, integrators must see the benign value — the risky one was
    // replaced in escrow and must not resurface.
    let fixture = setup();
    let wallet = Address::generate(&fixture.env);
    submit_into_escrow(&fixture, &wallet, 95, 90);

    // Past the 1-hour cooldown, still inside the 300s hold window.
    advance_to(&fixture, START_TS + 3_700);
    submit_into_escrow(&fixture, &wallet, 10, 90);

    let pending =
        fixture.ledgerlens.get_pending_score(&wallet, &symbol_short!("XLM_USDC")).unwrap();
    assert_eq!(pending.score, 10, "the benign value replaced the risky one");

    // The risky value must not be readable anywhere before the commit.
    assert_eq!(
        fixture.amm.try_swap(&wallet, &symbol_short!("XLM_USDC"), &1_000),
        Err(Ok(MockAmmError::HighRiskWallet))
    );

    advance_to(&fixture, START_TS + 3_700 + HOLD_WINDOW);
    fixture.ledgerlens.auto_commit_score(&wallet, &symbol_short!("XLM_USDC"));

    assert_eq!(fixture.amm.try_swap(&wallet, &symbol_short!("XLM_USDC"), &1_000), Ok(Ok(())));
}

// ── Validation parity reaches the external boundary ───────────────────────────

#[test]
fn out_of_range_score_never_reaches_escrow_or_external_calls() {
    // Regression guard: a score > 100 must be refused before it can be parked,
    // so an integrator can never observe one.
    let fixture = setup();
    let wallet = Address::generate(&fixture.env);

    let result = fixture.ledgerlens.try_submit_score(
        &Vec::new(&fixture.env),
        &wallet,
        &symbol_short!("XLM_USDC"),
        &101u32,
        &false,
        &false,
        &fixture.env.ledger().timestamp(),
        &90u32,
        &1u32,
        &None,
    );
    assert!(result.is_err());

    assert!(fixture.ledgerlens.get_pending_score(&wallet, &symbol_short!("XLM_USDC")).is_none());

    // And the wallet stays unborrowable / unswappable.
    assert_eq!(
        fixture.amm.try_swap(&wallet, &symbol_short!("XLM_USDC"), &1_000),
        Err(Ok(MockAmmError::HighRiskWallet))
    );
}

#[test]
fn low_confidence_committed_score_still_blocks_lending() {
    // Confidence is preserved through escrow: the lending market's floor is
    // applied against the committed value, not the raw score.
    let fixture = setup();
    let wallet = Address::generate(&fixture.env);
    // Score is safe (10 < 75) but confidence 20 is below MIN_CONFIDENCE (50).
    submit_into_escrow(&fixture, &wallet, 10, 20);

    advance_to(&fixture, START_TS + HOLD_WINDOW);
    fixture.ledgerlens.auto_commit_score(&wallet, &symbol_short!("XLM_USDC"));

    // Score-only gate passes; confidence-aware gate does not.
    assert!(fixture.ledgerlens.query_risk_gate(
        &wallet,
        &symbol_short!("XLM_USDC"),
        &GATE_THRESHOLD
    ));
    assert_eq!(
        fixture.lending.try_borrow(&wallet, &symbol_short!("XLM_USDC"), &1_000),
        Err(Ok(MockLendingError::RiskGateRejected))
    );
    assert_eq!(
        fixture.amm.try_provide_liquidity_gated(&wallet, &1_000),
        Err(Ok(MockAmmError::LowConfidence))
    );
}

// ── One wallet's escrow must not affect another ───────────────────────────────

#[test]
fn escrow_is_scoped_per_wallet() {
    let fixture = setup();
    let committed_wallet = Address::generate(&fixture.env);
    let parked_wallet = Address::generate(&fixture.env);

    submit_into_escrow(&fixture, &committed_wallet, 10, 90);
    advance_to(&fixture, START_TS + HOLD_WINDOW);
    fixture.ledgerlens.auto_commit_score(&committed_wallet, &symbol_short!("XLM_USDC"));

    // A second wallet submits after the window boundary, so its score parks.
    advance_to(&fixture, START_TS + HOLD_WINDOW + 3_601);
    submit_into_escrow(&fixture, &parked_wallet, 10, 90);

    assert_eq!(
        fixture.amm.try_swap(&committed_wallet, &symbol_short!("XLM_USDC"), &1_000),
        Ok(Ok(()))
    );
    assert_eq!(
        fixture.amm.try_swap(&parked_wallet, &symbol_short!("XLM_USDC"), &1_000),
        Err(Ok(MockAmmError::HighRiskWallet))
    );
}
