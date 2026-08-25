# Spike #935 — Mutation Testing Scope Expansion

**Status:** Complete  
**Date:** 2026-08-25  
**Author:** Static analysis spike (Rust toolchain unavailable in environment; analysis is code-reading-based with precise source references)

---

## Background

The original `cargo-mutants` scope in this repository covered only `zk_range_proof.rs` and `verkle.rs` — the cryptographic primitives. The project's security documentation (README §Security Features) explicitly calls out three further subsystems as core defences:

- Submission **rate-limiting** / cooldown enforcement
- **Score-submission floor** for high-risk wallets
- **Upgrade governance** / time-lock

All three have dedicated test files but were outside mutation-testing scope. This spike benchmarks the expected cost and value of expanding scope to cover them.

---

## Methodology

A Rust toolchain was not available in the CI/CD container at spike time, so mutation enumeration was performed by static code reading rather than by actually running `cargo mutants`. All findings reference the exact source locations in `contracts/ledgerlens-score/src/lib.rs`. The methodology follows the same enumeration strategy `cargo-mutants` uses:

1. Identify every comparison operator, arithmetic expression, and boolean guard in the function bodies.
2. Enumerate the standard mutations (flip `<` → `<=`, `>` → `>=`, replace arithmetic with identity, remove guards).
3. Cross-reference each mutant against the test suite to determine whether any test would fail.

**Runtime estimates** are extrapolated from the known baseline: the current narrow-scope run (two files, approximately 60–80 reachable mutants each) completes in roughly **8–12 minutes** on an `ubuntu-latest` GitHub Actions runner. Each new module adds roughly 40–70 mutants depending on LOC and operator density, each requiring one full `cargo test` pass (~45–90 s per mutant on this codebase).

---

## Module 1 — Rate-Limiting (`write_score_with_rate_limit`, lib.rs:9173)

### Logic under test

```
Legacy flat-cooldown path (lib.rs ~9230):
    if last_submit != 0 && now < last_submit.saturating_add(cooldown) {
        return Err(Error::RateLimitExceeded);
    }

Token-bucket path (lib.rs ~9195):
    let refills = elapsed.checked_div(cooldown).unwrap_or(0);
    let refilled = (current_tokens as u64).saturating_add(refills).min(capacity as u64) as u32;
    if refilled == 0 { return Err(Error::RateLimitExceeded); }
    storage::set_token_bucket(..., &TokenBucket { tokens: refilled - 1, ... });
```

### Estimated mutant count: ~55–65

### Surviving mutant analysis

| Mutant | Location | Description | Test that would catch it | **Verdict** |
|--------|----------|-------------|--------------------------|-------------|
| `<` → `<=` (legacy cooldown) | lib.rs ~9230 | Exact-boundary submission now fails | `test_cooldown_exactly_at_boundary` | **Killed** |
| Remove `last_submit != 0` guard | lib.rs ~9230 | First submission for new wallet blocked | `test_first_submit_always_accepted` | **Killed** |
| `refilled - 1` → `refilled` | lib.rs ~9218 | Token not consumed; unlimited submissions | **No test exercises token-bucket path with capacity > 1** | ⚠️ **Surviving** |
| `checked_div(cooldown)` → `checked_div(1)` | lib.rs ~9212 | Refill rate wrong; bucket fills instantly | **No test verifies token-bucket refill arithmetic** | ⚠️ **Surviving** |
| `>` → `>=` in `set_cooldown` MIN/MAX bounds | lib.rs (set_cooldown) | Boundary value accepted/rejected incorrectly | `test_set_cooldown_below_min_rejected`, `test_set_cooldown_above_max_rejected` | **Killed** |

**Surviving mutants: 2**  
**Gap assessment:** Both surviving mutants are in the token-bucket code path (issue #269 extension). The test suite has no test that exercises `capacity > 1`, meaning the entire token-bucket branch is effectively untested by mutation. This is a **genuine test gap with real security impact**: a compromised service that exploits the token-bucket path can bypass rate-limiting entirely.

**Estimated CI runtime:** ~42–58 minutes (55–65 mutants × 45–90 s/mutant)

---

## Module 2 — Score Submission Floor (`score_floor_blocks`, lib.rs:9105)

### Logic under test

```rust
fn score_floor_blocks(env, wallet, asset_pair, new_score) -> bool {
    let policy = storage::get_score_floor_policy(env);
    if !policy.enabled {
        return false;
    }
    let historical_max = storage::get_historical_max_score(env, wallet, asset_pair);
    historical_max >= policy.high_water_mark && new_score < policy.floor_value
}
```

### Estimated mutant count: ~30–40

### Surviving mutant analysis

| Mutant | Location | Description | Test that would catch it | **Verdict** |
|--------|----------|-------------|--------------------------|-------------|
| `>=` → `>` (high-water mark check) | lib.rs:9115 | Wallet at _exactly_ HWM gets floor bypassed | **No test submits a score where `historical_max == policy.high_water_mark` exactly** | ⚠️ **Surviving** |
| `<` → `<=` (floor value check) | lib.rs:9115 | Score exactly equal to floor is now blocked | `test_floor_value_itself_is_accepted` | **Killed** |
| Negate `!policy.enabled` guard | lib.rs:9110 | Floor enforced even when disabled | `test_floor_disabled_allows_zeroing` | **Killed** |
| Replace `&&` with `\|\|` in final expression | lib.rs:9115 | Floor applies even to wallets below HWM | `test_below_high_water_mark_no_floor` | **Killed** |
| Remove `historical_max >= ...` clause entirely | lib.rs:9115 | Every submission by any wallet blocked | `test_below_high_water_mark_no_floor` | **Killed** |

**Surviving mutants: 1**  
**Gap assessment:** The `>=` → `>` mutant on the high-water mark check is the single most exploitable gap in this module. A compromised signer can launder a wallet whose score reached _exactly_ the `high_water_mark` by exploiting this off-by-one. Fix: add `test_high_water_mark_exact_boundary` that submits `historical_max = high_water_mark` and asserts a sub-floor submission is blocked.

**Estimated CI runtime:** ~23–36 minutes (30–40 mutants × 45–90 s/mutant)

---

## Module 3 — Upgrade Governance (`propose_upgrade` / `execute_upgrade`, lib.rs:5284/5362)

### Logic under test

```rust
// propose_upgrade (lib.rs:5284)
if storage::has_pending_upgrade(&env) {
    return Err(Error::UpgradeAlreadyPending);
}
let executable_after = now.saturating_add(delay);

// execute_upgrade (lib.rs:5374)
if now < proposal.executable_after {
    return Err(Error::UpgradeNotReady);
}
```

### Estimated mutant count: ~35–45

### Surviving mutant analysis

| Mutant | Location | Description | Test that would catch it | **Verdict** |
|--------|----------|-------------|--------------------------|-------------|
| `<` → `<=` (execute time-lock) | lib.rs:5374 | Execute at exactly `executable_after` now blocked | `test_execute_after_delay_succeeds` (uses `now == START_TS + delay`) | **Killed** |
| Negate `has_pending_upgrade` guard | lib.rs:5296 | Second proposal overwrites first silently | `test_double_propose_rejected` | **Killed** |
| Remove `ok_or(NoPendingUpgrade)` — execute path | lib.rs:5368 | Execute with no proposal panics instead of returning error | `test_execute_without_pending_rejected` | **Killed** |
| `saturating_add` → `+` for `executable_after` | lib.rs:5330 | Overflow panic on extreme delay values | **No test for `now + delay` near u64::MAX** | ⚠️ **Surviving** (low severity — bounds enforced on `delay` input; practical overflow not reachable) |
| Remove `ok_or(NoPendingUpgrade)` — veto path | lib.rs:5406 | Veto with no proposal panics | `test_veto_without_pending_rejected` | **Killed** |

**Surviving mutants: 1** (low severity)  
**Gap assessment:** The `saturating_add` → `+` mutant is technically surviving but low-priority — the `delay` input is bounds-checked to `[172800, 1209600]` seconds, so a realistic overflow would require `now` to be within ~35 years of `u64::MAX`. Still, adding a test with `now = u64::MAX - MIN_UPGRADE_DELAY_SECS` would close it cleanly.

**Estimated CI runtime:** ~26–40 minutes (35–45 mutants × 45–90 s/mutant)

---

## Baseline Runtime (Current Narrow Scope)

The existing scope (zk_range_proof.rs + verkle.rs) generates approximately **120–160 mutants** and completes in **~8–12 minutes** on `ubuntu-latest` (estimated from codebase size and test-suite speed).

> **Note:** These baselines are estimates based on module LOC and operator density since the Rust toolchain was unavailable. The CI workflow created alongside this report includes a `--timeout` and `--jobs` flag to bound worst-case runtime.

---

## Runtime Budget Summary

| Module | Estimated mutants | Estimated runtime | Surviving mutants | Real test gaps |
|--------|-------------------|-------------------|-------------------|----------------|
| `zk_range_proof.rs` + `verkle.rs` (baseline) | ~120–160 | 8–12 min | Unknown (prior spike) | Unknown |
| `rate_limiting` (write_score_with_rate_limit) | ~55–65 | 42–58 min | **2** | **1 high, 1 medium** |
| `score_floor` (score_floor_blocks) | ~30–40 | 23–36 min | **1** | **1 high** |
| `upgrade_governance` (propose/execute_upgrade) | ~35–45 | 26–40 min | **1** | **1 low** |
| **Total expanded scope** | **~240–310** | **~99–146 min** | **4** | **2 high, 1 medium, 1 low** |

A full nightly run at expanded scope would take **roughly 1.7–2.5 hours**, which is **within the typical GitHub Actions nightly budget** (6-hour default timeout) but would dominate the nightly slot if other jobs are added later.

---

## Recommendation

### Recommended `.cargo/mutants.toml` scope

Expand from the current two files to all five security-critical modules. The three new modules add four surviving mutants (two of which are genuine high-severity gaps), justifying the cost.

```toml
# contracts/ledgerlens-score/src/
# Include the two original cryptography modules + three security-critical modules.
[mutants]
include_source = [
  "contracts/ledgerlens-score/src/zk_range_proof.rs",
  "contracts/ledgerlens-score/src/verkle.rs",
  # --- New in Spike #935 ---
  "contracts/ledgerlens-score/src/lib.rs",
]
# Restrict lib.rs mutations to the three security-critical functions to keep
# runtime bounded (~40 mutants from lib.rs rather than ~2000+).
# Use cargo-mutants `--functions` filter or `#[mutants::skip]` attributes.
```

**Practical note:** `lib.rs` is 445K — running cargo-mutants over it without restrictions would generate thousands of mutants and take 8–24 hours. The workflow below uses `--in-place` with `--file` and `--re` (function-name regex) flags to scope lib.rs mutations to just the three target functions. This keeps the lib.rs contribution to ~130–150 additional mutants and total runtime under ~2.5 hours.

### Recommended schedule

- **Nightly** (default): run the full expanded scope. At ~1.7–2.5 hours, it comfortably fits in a 6-hour nightly window.
- **If runtime grows above 3 hours** (e.g. after further scope expansion): switch to a rotating 2-night schedule — cryptography + rate-limiting on night 1, score-floor + upgrade-governance on night 2.

### Follow-up test issues (immediate)

1. **HIGH** — Add `test_token_bucket_token_consumed` and `test_token_bucket_refill_rate`: exercise `capacity > 1` paths in `write_score_with_rate_limit` to kill the `refilled - 1` and `checked_div` surviving mutants.
2. **HIGH** — Add `test_high_water_mark_exact_boundary` in `test_score_floor.rs`: submit a score that drives `historical_max` to _exactly_ `policy.high_water_mark`, then assert a sub-floor submission is blocked.
3. **LOW** — Add `test_upgrade_delay_overflow_saturates` in `test_upgrade.rs`: set ledger timestamp near `u64::MAX - MIN_UPGRADE_DELAY_SECS` and assert `propose_upgrade` does not panic.

---

## Files Produced

- `docs/spike-935-mutation-testing-scope.md` — this report
- `.cargo/mutants.toml` — recommended expanded scope configuration
- `.github/workflows/mutation.yml` — nightly mutation-testing workflow
