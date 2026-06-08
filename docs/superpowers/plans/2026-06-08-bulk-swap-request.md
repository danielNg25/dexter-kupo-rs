# BulkSwapRequest Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `BulkSwapRequest` builder + `BulkOrderPlan` return type + `DexSwap::build_bulk_orders` trait method to `dexter-kupo-rs`. Three explicit action kinds (`add_swap`, `add_cancel`, `add_update`) that consumers can combine into one transaction's worth of intent. Plus an `examples/live_bulk.rs` that builds + signs + submits a real bundled tx on mainnet.

**Architecture:** A bundle is structurally: many script inputs (cancels, each with the cancel redeemer + shared reference script) + many new script outputs (places, each with inline datum). Pure cancels, pure places, and updates all flatten into the same `BulkOrderPlan { cancels: Vec<SpendUtxo>, new_orders: Vec<PayToAddress>, return_address }`. The bundle's tx-layer structure doesn't care which action kind any item came from.

**Tech Stack:** Rust, existing dexter-kupo-rs primitives (SwapRequest / CancelSwapRequest internally). For the live example: `pallas-txbuilder`, `pallas-primitives`, `pallas-crypto`, `pallas-addresses`, `pallas-codec` (already deps).

---

## File structure

```
src/requests/
├── bulk.rs                # NEW: BulkSwapRequest + BulkOrderPlan
├── mod.rs                 # MODIFIED: pub mod bulk; pub use bulk::*;
├── swap.rs                # unchanged
├── cancel.rs              # unchanged
├── update.rs              # unchanged
└── types.rs               # unchanged

src/dex/
├── swap.rs                # MODIFIED: add build_bulk_orders trait method
├── minswap_v2_swap.rs     # unchanged (uses default impl)
└── ...                    # unchanged

src/lib.rs                 # MODIFIED: re-export BulkSwapRequest + BulkOrderPlan

tests/
└── bulk_request.rs        # NEW: integration tests

examples/
└── live_bulk.rs           # NEW: mainnet end-to-end (1 place + 1 cancel + 1 update)

Cargo.toml                 # MODIFIED: register live_bulk example
```

---

### Task 1: Add `BulkOrderPlan` struct + `BulkSwapRequest` builder skeleton

**Files:**
- Create: `src/requests/bulk.rs`
- Modify: `src/requests/mod.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Write failing test for builder shape**

Create `tests/bulk_request.rs`:

```rust
use dexter_kupo_rs::{
    dex::minswap_v2::MinswapV2,
    BulkSwapRequest, KupoApi,
};

#[test]
fn empty_bundle_errors() {
    let dex = MinswapV2::new(KupoApi::new("http://localhost:1442"));
    let err = BulkSwapRequest::new(&dex)
        .with_return_address("addr1q8n0za95rmvwt45mvxc6lpedh5wx0jl9pyuyfufgzslfdg7tkywsce2nu7xc2pukfsl08x9wlmlnka59c5xcxjamsgfsnyaqug")
        .expect("addr")
        .build()
        .unwrap_err();
    assert!(format!("{err:#}").contains("empty bundle"));
}

#[test]
fn missing_return_address_errors() {
    let dex = MinswapV2::new(KupoApi::new("http://localhost:1442"));
    // Skip with_return_address. add_cancel with a synthetic utxo.
    let err = BulkSwapRequest::new(&dex)
        .add_cancel(crate_test::synthetic_order_utxo())
        .build()
        .unwrap_err();
    assert!(format!("{err:#}").contains("return_address"));
}

mod crate_test {
    use dexter_kupo_rs::models::{Unit, Utxo};
    pub fn synthetic_order_utxo() -> Utxo {
        Utxo {
            address: "addr1z8p79rpkcdz8x9d6tft0x0dx5mwuzac2sa4gm8cvkw5hcn8ftv3526r2y8z8rnlvay76nhpdp9pdekzvd3rrrql08qqssmhdxz".into(),
            tx_hash: "aa".repeat(32),
            tx_index: 0,
            output_index: 0,
            amount: vec![Unit { unit: "lovelace".into(), quantity: "5000000".into() }],
            block: String::new(),
            data_hash: None,
            inline_datum: None,
            reference_script_hash: None,
            datum_type: None,
        }
    }
}
```

- [ ] **Step 2: Run to verify failure**

```bash
cd /Users/daniel/WorkStation/Personal/Trading/Thai/dexter/dexter-kupo-rs
cargo test --test bulk_request 2>&1 | tail -10
```
Expected: compile error — `BulkSwapRequest` doesn't exist.

- [ ] **Step 3: Create `src/requests/bulk.rs`** with skeleton:

```rust
//! Fluent builder for a single Cardano transaction bundling multiple
//! Minswap V2 order actions: fresh places, pure cancels, and atomic
//! cancel+place updates.
//!
//! See spec: `docs/superpowers/specs/2026-06-08-bulk-swap-request-design.md`.

use anyhow::{anyhow, Result};

use crate::dex::DexSwap;
use crate::models::Utxo;
use crate::requests::types::{PayToAddress, SpendUtxo, SwapParams};

/// What the bulk builder hands back. Flat from the caller's perspective:
/// all script inputs in `cancels`, all script outputs in `new_orders`.
/// Pure-cancel, pure-place, and update intents all collapse into this shape.
#[derive(Debug, Clone)]
pub struct BulkOrderPlan {
    pub cancels: Vec<SpendUtxo>,
    pub new_orders: Vec<PayToAddress>,
    pub return_address: String,
}

pub struct BulkSwapRequest<'a, D: DexSwap + ?Sized> {
    dex: &'a D,
    return_address: Option<String>,
    cancels: Vec<Utxo>,
    swaps: Vec<SwapParams>,
    updates: Vec<(Utxo, SwapParams)>,
}

impl<'a, D: DexSwap + ?Sized> BulkSwapRequest<'a, D> {
    pub fn new(dex: &'a D) -> Self {
        Self { dex, return_address: None, cancels: vec![], swaps: vec![], updates: vec![] }
    }

    pub fn with_return_address(mut self, addr: &str) -> Result<Self> {
        self.return_address = Some(addr.to_string());
        Ok(self)
    }

    pub fn add_cancel(mut self, utxo: Utxo) -> Self {
        self.cancels.push(utxo);
        self
    }

    pub fn add_swap(mut self, params: SwapParams) -> Self {
        self.swaps.push(params);
        self
    }

    pub fn add_update(mut self, old_utxo: Utxo, new_params: SwapParams) -> Self {
        self.updates.push((old_utxo, new_params));
        self
    }

    pub fn build(self) -> Result<BulkOrderPlan> {
        let return_address = self.return_address
            .ok_or_else(|| anyhow!("BulkSwapRequest: return_address not set"))?;
        if self.cancels.is_empty() && self.swaps.is_empty() && self.updates.is_empty() {
            return Err(anyhow!("BulkSwapRequest: empty bundle (need at least one action)"));
        }
        self.dex.build_bulk_orders(&self.cancels, &self.swaps, &self.updates, &return_address)
    }
}
```

- [ ] **Step 4: Wire re-exports**

`src/requests/mod.rs`: add `pub mod bulk; pub use bulk::{BulkSwapRequest, BulkOrderPlan};`

`src/lib.rs`: add `pub use requests::{BulkSwapRequest, BulkOrderPlan};` alongside the existing re-exports.

- [ ] **Step 5: Add the trait method to `DexSwap` with a stub**

In `src/dex/swap.rs`, add a method that returns an error for now (Task 2 implements the default impl):

```rust
fn build_bulk_orders(
    &self,
    _cancels: &[crate::models::Utxo],
    _swaps: &[crate::requests::types::SwapParams],
    _updates: &[(crate::models::Utxo, crate::requests::types::SwapParams)],
    _return_address: &str,
) -> anyhow::Result<crate::requests::bulk::BulkOrderPlan> {
    anyhow::bail!("DexSwap::build_bulk_orders default impl not yet wired (Task 2)")
}
```

- [ ] **Step 6: Verify the two tests now COMPILE (one still fails, one passes)**

The `empty_bundle_errors` test should pass — empty bundle is detected before reaching `build_bulk_orders`. The `missing_return_address_errors` test should also pass.

```bash
cargo test --test bulk_request 2>&1 | tail -10
```
Expected: 2 passed.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(requests/bulk): BulkSwapRequest skeleton + BulkOrderPlan type

Public-facing builder with add_cancel / add_swap / add_update fluent
methods. Validates non-empty bundle + return_address before delegating
to the DexSwap trait. Trait method stub bails; Task 2 implements the
default impl that composes the existing per-item builders."
```

---

### Task 2: Implement `DexSwap::build_bulk_orders` default impl

**Files:**
- Modify: `src/dex/swap.rs`

- [ ] **Step 1: Write failing test**

Append to `tests/bulk_request.rs`:

```rust
use dexter_kupo_rs::{
    dex::{minswap_v2::MinswapV2, DexSwap},
    models::{Asset, LiquidityPool, Token, Unit, Utxo, token_identifier},
    requests::types::SwapParams,
    BulkSwapRequest, KupoApi,
};

fn pool() -> LiquidityPool {
    LiquidityPool {
        dex_identifier: "MinswapV2".into(),
        asset_a: Token::Lovelace,
        asset_b: Token::Asset(Asset::new(
            "f13ac4d66b3ee19a6aa0f2a22298737bd907cc9512166f2fc971b527",
            "535452494b45", 0,
        )),
        reserve_a: 1_000_000_000_000,
        reserve_b: 5_000_000_000_000,
        address: "addr1...".into(),
        pool_id: format!("{}{}", "f5808c2c990d86da54bfc97d89cee6efa20cd8461616359478d96b4c",
                                 "7dd6988c5a86693c76aeec1ea94afa41770be0de21a775ca7a2a1eabdb6a0171"),
        pool_fee_percent: 0.3,
        total_lp_tokens: 0,
    }
}

fn sender_bech32() -> &'static str {
    "addr1q8n0za95rmvwt45mvxc6lpedh5wx0jl9pyuyfufgzslfdg7tkywsce2nu7xc2pukfsl08x9wlmlnka59c5xcxjamsgfsnyaqug"
}

fn synthetic_swap_params() -> SwapParams {
    use dexter_kupo_rs::address::decode_base_address;
    use dexter_kupo_rs::requests::types::OrderKind;
    let addr = decode_base_address(sender_bech32()).expect("addr");
    SwapParams {
        sender: addr.clone(),
        receiver: addr,
        swap_in_token: Token::Lovelace,
        swap_out_token: Token::Asset(Asset::new(
            "f13ac4d66b3ee19a6aa0f2a22298737bd907cc9512166f2fc971b527",
            "535452494b45", 0,
        )),
        swap_in_amount: 100_000_000,
        min_receive: 400_000_000,
        kind: OrderKind::Limit,
        kill_on_failed: false,
        spend_utxos: vec![],
    }
}

#[test]
fn bulk_of_one_swap_matches_single_swap_request() {
    let dex = MinswapV2::new(KupoApi::new("http://localhost:1442"));
    let params = synthetic_swap_params();
    let bulk = BulkSwapRequest::new(&dex)
        .with_return_address(sender_bech32()).expect("addr")
        .add_swap(params.clone())
        .build()
        .expect("bulk build");
    assert_eq!(bulk.cancels.len(), 0);
    assert_eq!(bulk.new_orders.len(), 1);

    let single = dex.build_swap_order(&pool(), &params).expect("single build");
    assert_eq!(bulk.new_orders[0].datum, single[0].datum,
               "bulk-of-1 swap must produce identical datum to single SwapRequest");
}

#[test]
fn bulk_of_one_cancel_matches_single_cancel_request() {
    let dex = MinswapV2::new(KupoApi::new("http://localhost:1442"));
    let order_utxo = crate_test::synthetic_order_utxo();
    let bulk = BulkSwapRequest::new(&dex)
        .with_return_address(sender_bech32()).expect("addr")
        .add_cancel(order_utxo.clone())
        .build()
        .expect("bulk build");
    assert_eq!(bulk.cancels.len(), 1);
    assert_eq!(bulk.new_orders.len(), 0);

    let single = dex.build_cancel_order(&[order_utxo], sender_bech32()).expect("single cancel");
    assert_eq!(bulk.cancels[0].utxo.tx_hash, single[0].spend_utxos[0].utxo.tx_hash);
    assert_eq!(bulk.cancels[0].redeemer, single[0].spend_utxos[0].redeemer);
    assert_eq!(bulk.cancels[0].validator_reference, single[0].spend_utxos[0].validator_reference);
}

#[test]
fn bulk_of_one_update_has_cancel_plus_new_order() {
    let dex = MinswapV2::new(KupoApi::new("http://localhost:1442"));
    let bulk = BulkSwapRequest::new(&dex)
        .with_return_address(sender_bech32()).expect("addr")
        .add_update(crate_test::synthetic_order_utxo(), synthetic_swap_params())
        .build()
        .expect("bulk build");
    assert_eq!(bulk.cancels.len(), 1, "update produces one cancel input");
    assert_eq!(bulk.new_orders.len(), 1, "update produces one new output");
}

#[test]
fn bulk_mixed_three_kinds() {
    let dex = MinswapV2::new(KupoApi::new("http://localhost:1442"));
    let bulk = BulkSwapRequest::new(&dex)
        .with_return_address(sender_bech32()).expect("addr")
        .add_swap(synthetic_swap_params())
        .add_cancel(crate_test::synthetic_order_utxo())
        .add_update(crate_test::synthetic_order_utxo(), synthetic_swap_params())
        .build()
        .expect("bulk build");
    // cancels = 1 from add_cancel + 1 from add_update = 2
    // new_orders = 1 from add_swap + 1 from add_update = 2
    assert_eq!(bulk.cancels.len(), 2);
    assert_eq!(bulk.new_orders.len(), 2);
}
```

- [ ] **Step 2: Verify failure**

```bash
cargo test --test bulk_request 2>&1 | tail -15
```
Expected: 4 of the 6 tests fail with the "default impl not yet wired" bail.

- [ ] **Step 3: Implement the default trait method**

In `src/dex/swap.rs`, replace the Task 1 stub:

```rust
/// Bundle multiple Minswap V2 order actions into a single BulkOrderPlan.
///
/// Default impl composes per-item via `build_swap_order` and
/// `build_cancel_order`:
/// - each cancel UTxO → one `SpendUtxo` (via build_cancel_order's spend_utxos)
/// - each swap_params → one new `PayToAddress` (via build_swap_order, with
///   its own spend_utxos cleared since the bundle has none-per-output)
/// - each (old_utxo, new_params) update → both, lifted to top-level cancels
///   and new_orders
///
/// Equivalent on-chain structure for one cancel + one swap == one update.
/// The distinction is preserved in the BUILDER intent only.
fn build_bulk_orders(
    &self,
    cancels: &[crate::models::Utxo],
    swaps: &[crate::requests::types::SwapParams],
    updates: &[(crate::models::Utxo, crate::requests::types::SwapParams)],
    return_address: &str,
) -> anyhow::Result<crate::requests::bulk::BulkOrderPlan> {
    use crate::requests::bulk::BulkOrderPlan;

    let mut all_cancels = Vec::new();
    let mut all_new_orders = Vec::new();

    // Pure cancels: extract their SpendUtxo via build_cancel_order.
    if !cancels.is_empty() {
        let pays = self.build_cancel_order(cancels, return_address)?;
        for p in pays {
            for spend in p.spend_utxos {
                all_cancels.push(spend);
            }
        }
    }

    // Pure swaps: each → its own PayToAddress (with spend_utxos cleared —
    // the bundle's cancels live at top-level, not per-output).
    for swap in swaps {
        let pays = self.build_swap_order(&swap.pool.clone(), swap)?;
        for mut p in pays {
            p.spend_utxos.clear();
            all_new_orders.push(p);
        }
    }

    // Updates: cancel-half goes into all_cancels, place-half goes into
    // all_new_orders. Structurally identical to add_cancel + add_swap.
    for (old_utxo, new_params) in updates {
        let cancel_pays = self.build_cancel_order(&[old_utxo.clone()], return_address)?;
        for p in cancel_pays {
            for spend in p.spend_utxos {
                all_cancels.push(spend);
            }
        }
        let swap_pays = self.build_swap_order(&new_params.pool.clone(), new_params)?;
        for mut p in swap_pays {
            p.spend_utxos.clear();
            all_new_orders.push(p);
        }
    }

    Ok(BulkOrderPlan {
        cancels: all_cancels,
        new_orders: all_new_orders,
        return_address: return_address.to_string(),
    })
}
```

NOTE: this impl assumes `SwapParams` carries the `pool` field (currently it does — `SwapParams.pool: LiquidityPool`). Confirm by reading `src/requests/types.rs`.

- [ ] **Step 4: Run tests**

```bash
cargo test --test bulk_request 2>&1 | tail -15
cargo test 2>&1 | grep "test result" | tail -10
```
Expected: all 6 bulk tests pass; the full suite (~60 tests) still passes.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(dex): DexSwap::build_bulk_orders default impl

Composes build_swap_order + build_cancel_order per item, then aggregates
into BulkOrderPlan. Updates produce one cancel-half + one place-half,
structurally identical to add_cancel + add_swap — the distinction is
preserved at the builder layer for caller intent only.

4 new bulk-vs-single equivalence tests verify the composition produces
byte-identical SpendUtxos and datums to the existing per-action builders."
```

---

### Task 3: Add `examples/live_bulk.rs` — mainnet end-to-end

**Files:**
- Create: `examples/live_bulk.rs`
- Modify: `Cargo.toml`

This example is the integration test for tx assembly. It builds a real bundle (1 cancel + 1 swap + 1 update) and submits to mainnet via Blockfrost. Reuses all the patterns from `examples/live_update.rs` for tx assembly + signing + submission.

- [ ] **Step 1: Read prior art**

Read `examples/live_update.rs` in full. It already shows:
- BIP-39 key derivation
- Pinned `COLLATERAL_UTXO` env var
- Blockfrost params + evaluate
- Reference script + Conway ref-script fee
- Fixpoint fee loop
- Signing + dry-run/--submit

The bundle example mirrors that structure but:
- Iterates the `BulkOrderPlan.cancels` Vec for ALL script inputs (not just one)
- Iterates `new_orders` for ALL outputs (not just one)
- Blockfrost evaluate returns N spend redeemers (one per script input); collect them
- The fee math sums all script fees + ONE reference-script fee

- [ ] **Step 2: Create `examples/live_bulk.rs`**

The full file is ~600-700 lines (heavier than live_update since it iterates instead of single-output). Crib from `examples/live_update.rs`. New env vars: same as live_update, plus optional CLI flags:

```
USAGE:
  cargo run --example live_bulk -- \
    --cancel <tx_hash>#<idx> \             # repeatable
    --swap-min-receive <u64> \             # for each new place
    --update <old_tx_hash>#<idx> \         # repeatable: cancel-half of update
    [--submit]
```

For simplicity, v1 of the example uses ENV-driven swap params (re-uses SWAP_IN_UNIT, SWAP_IN_AMOUNT, LIMIT_PREMIUM_PERCENT) for every new place and update — i.e., all "places" in this example use the same params. The bot's executor (separate plan) will configure per-action params.

- [ ] **Step 3: Register in Cargo.toml**

```toml
[[example]]
name = "live_bulk"
path = "examples/live_bulk.rs"
```

- [ ] **Step 4: Build + dry-run smoke**

```bash
cargo build --example live_bulk 2>&1 | tail -3
```
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(example): live_bulk — submit bundled tx (places + cancels + updates) on mainnet

End-to-end demonstration of BulkSwapRequest. Mirrors live_update.rs
patterns: inline datum on new outputs, reference script + Conway
ref-script fee (paid ONCE for the whole bundle), Blockfrost evaluate
for N redeemers, fixpoint fee loop, pinned COLLATERAL_UTXO,
disclosed_signer, language_views. --submit broadcasts; default dry-run."
```

---

### Task 4: Final whole-branch review + handoff

**Files:** none (verification only)

- [ ] **Step 1: Full test suite**

```bash
cargo test 2>&1 | grep "test result" | tail -10
```
Expected: all pass (~64 tests after Tasks 1-3 add ~6 new).

- [ ] **Step 2: Clippy on the new code**

```bash
cargo clippy --all-targets 2>&1 | grep -E "warning|error" | head -20
```
Note new warnings, fix any that are non-trivial.

- [ ] **Step 3: User runs the mainnet smoke**

```bash
# Dry-run first
cargo run --example live_bulk -- \
  --cancel <real_test_tx>#0 \
  --update <another_real_test_tx>#0

# Then broadcast
cargo run --example live_bulk -- \
  --cancel <real_test_tx>#0 \
  --update <another_real_test_tx>#0 \
  --submit
```

User compares the bundled tx fee against what 3 separate txs would have cost.

---

## Done criteria

- All ~64 tests pass (existing + 6 new bulk_request tests)
- `examples/live_bulk.rs` builds and runs in dry-run mode without panics
- User verifies live mainnet bundle tx submission succeeds + costs ~50% of equivalent separate-txs total
- Branch ready for finishing-branch flow → merge to master
