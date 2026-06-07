# UpdateSwapRequest + Reference Script Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `UpdateSwapRequest` to `dexter-kupo-rs` that bundles cancel + place into a single atomic transaction, plus reference-script support so `Cancel` and `Update` use Minswap's on-chain published script reference instead of carrying the 2659-byte script inline. Together these match Minswap UI's fee profile (~0.25 ADA per update, ~0.20 ADA per cancel).

**Architecture:** Investigation of tx `ddb038fae5556b564b04e9dcf6139415d8be3b2e45a4e8c8eba29df79e47d169` confirmed Minswap V2 has NO contract-level "update" redeemer — the UI's "update" is a tx-level pattern: spend the old order with the standard cancel redeemer (`d87a80`) + create a new order in the same tx's outputs + use the published script reference at `cf4ecddde0d81f9ce8fcc881a85eb1f8ccdaf6807f03fea4cd02da896a621776#0` instead of inlining the 2659-byte validator. We mirror that pattern.

**Tech Stack:** Rust, existing dexter-kupo-rs primitives, pallas-txbuilder (for examples)

---

## Background: what the on-chain "update" tx actually does

From inspection of `ddb038fae5556b564b04e9dcf6139415d8be3b2e45a4e8c8eba29df79e47d169`:

| Element | Value |
|---|---|
| Total fee | 252,427 lovelace (~0.25 ADA) |
| Tx size | 980 bytes |
| Inputs | 1 wallet UTxO + 1 SCRIPT UTxO (the old order at `addr1z8p79rpk...`) |
| Outputs | 1 wallet change + 1 NEW order at same script address (different swap params) |
| Reference inputs | `cf4ecddde0d81f9ce8fcc881a85eb1f8ccdaf6807f03fea4cd02da896a621776#0` holding the V2 order script (hash `c3e28c36c3447315ba5a56f33da6a6ddc1770a876a8d9f0cb3a97c4c`, size 2659) |
| Redeemer | `{constructor: 1, fields: []}` = CBOR `d87a80` (the SAME cancel redeemer) |
| Collateral | provided |

There is no separate update redeemer in the contract — the bundle is purely a transaction-level pattern.

---

### Task 1: Add `UtxoRef` type + `validator_reference` field on `SpendUtxo`

**Files:**
- Modify: `src/requests/types.rs`

**Why:** Lets the library signal "use this on-chain UTxO as a script reference (CIP-33)" without forcing the executor to know which DEX has published refs where.

- [ ] **Step 1: Write the failing test**

In `src/requests/types.rs`, add at bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utxo_ref_serializes_with_all_fields() {
        let r = UtxoRef {
            tx_hash: "cf4ecddde0d81f9ce8fcc881a85eb1f8ccdaf6807f03fea4cd02da896a621776".into(),
            output_index: 0,
            script_hash: "c3e28c36c3447315ba5a56f33da6a6ddc1770a876a8d9f0cb3a97c4c".into(),
        };
        assert_eq!(r.tx_hash.len(), 64);
        assert_eq!(r.output_index, 0);
        assert_eq!(r.script_hash.len(), 56);
    }

    #[test]
    fn spend_utxo_validator_reference_defaults_to_none() {
        // Existing constructions should still compile and have None for the new field.
        let s = SpendUtxo {
            utxo: crate::models::Utxo::default(),
            redeemer: None,
            validator: None,
            validator_reference: None,
            signer: None,
        };
        assert!(s.validator_reference.is_none());
    }
}
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cd /Users/daniel/WorkStation/Personal/Trading/Thai/dexter/dexter-kupo-rs && cargo test --lib requests::types::tests`
Expected: compile errors — `UtxoRef` undefined, `validator_reference` not a field.

- [ ] **Step 3: Add `UtxoRef` struct + new field**

Add to `src/requests/types.rs`:

```rust
/// On-chain reference to a UTxO that carries a Plutus script as its
/// reference_script (CIP-33). The executor can attach this UTxO as a
/// `reference_input` instead of including the script bytes in the witness set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UtxoRef {
    pub tx_hash: String,
    pub output_index: u64,
    /// The Blake2b-224 hash of the script at this UTxO. The executor verifies
    /// the reference matches before using it.
    pub script_hash: String,
}
```

And extend `SpendUtxo`:

```rust
pub struct SpendUtxo {
    pub utxo: Utxo,
    pub redeemer: Option<String>,
    /// Inline validator (full script CBOR). Use this when no reference UTxO
    /// is available. Either `validator` or `validator_reference` should be
    /// populated; if both are Some, executors should prefer `validator_reference`.
    pub validator: Option<PlutusScript>,
    /// On-chain script reference (CIP-33). When Some, executors should attach
    /// this UTxO as a `reference_input` and skip the inline script.
    pub validator_reference: Option<UtxoRef>,
    pub signer: Option<String>,
}
```

- [ ] **Step 4: Update all existing constructors of `SpendUtxo` to set `validator_reference: None`**

Grep: `grep -rn "SpendUtxo {" src/ tests/` and add `validator_reference: None` to each.

- [ ] **Step 5: Run tests to verify pass**

Run: `cargo test --lib`
Expected: all tests pass, including the two new ones.

- [ ] **Step 6: Commit**

```bash
git add src/requests/types.rs
git commit -m "feat(types): add UtxoRef + validator_reference field on SpendUtxo

Lets the library signal CIP-33 script references for Plutus spends.
Executors prefer validator_reference when both are set; legacy executors
that ignore the field still work via the inline validator fallback."
```

---

### Task 2: Add `ORDER_SCRIPT_REF_UTXO` constant + populate it in `MinswapV2::build_cancel_order`

**Files:**
- Modify: `src/dex/minswap_v2_swap.rs`

- [ ] **Step 1: Add the constant near the existing `ORDER_SCRIPT_HASH`**

```rust
/// The on-chain UTxO that holds the Minswap V2 order script as its
/// reference_script. Mainnet only. Discovered from tx
/// `ddb038fae5556b564b04e9dcf6139415d8be3b2e45a4e8c8eba29df79e47d169`.
pub const ORDER_SCRIPT_REF_UTXO_TX: &str =
    "cf4ecddde0d81f9ce8fcc881a85eb1f8ccdaf6807f03fea4cd02da896a621776";
pub const ORDER_SCRIPT_REF_UTXO_INDEX: u64 = 0;
```

- [ ] **Step 2: Populate `validator_reference` in `build_cancel_order`**

Find the existing `SpendUtxo { ... }` construction inside `build_cancel_order` and add:

```rust
spend_utxos: vec![SpendUtxo {
    utxo: order_utxo.clone(),
    redeemer: Some(CANCEL_REDEEMER.to_string()),
    validator: Some(PlutusScript {
        version: PlutusVersion::V2,
        cbor_hex: ORDER_SCRIPT_CBOR_HEX.to_string(),
    }),
    validator_reference: Some(UtxoRef {
        tx_hash: ORDER_SCRIPT_REF_UTXO_TX.to_string(),
        output_index: ORDER_SCRIPT_REF_UTXO_INDEX,
        script_hash: ORDER_SCRIPT_HASH.to_string(),
    }),
    signer: Some(return_address.to_string()),
}],
```

Both `validator` (inline fallback) AND `validator_reference` are set. Executors choose.

- [ ] **Step 3: Update the cancel test to assert `validator_reference` is populated**

In the existing `build_cancel_order_picks_the_order_utxo_and_emits_redeemer` test, add:

```rust
assert_eq!(
    s.validator_reference.as_ref().map(|r| (r.tx_hash.as_str(), r.output_index)),
    Some(("cf4ecddde0d81f9ce8fcc881a85eb1f8ccdaf6807f03fea4cd02da896a621776", 0))
);
```

- [ ] **Step 4: Run tests**

Run: `cargo test --lib`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add src/dex/minswap_v2_swap.rs
git commit -m "feat(dex/minswap_v2): populate validator_reference for cancel + add ref UTxO constants

Both validator (inline) and validator_reference (CIP-33) are now set on
cancel SpendUtxos. Executors that read the reference field can skip
shipping the 2659-byte script in the witness set, saving ~0.10 ADA per
cancel. The reference UTxO is on mainnet; testnet executors fall back to
the inline validator."
```

---

### Task 3: Add `build_update_order` default impl on `DexSwap` trait

**Files:**
- Modify: `src/dex/swap.rs`

- [ ] **Step 1: Write the failing test**

Add to `src/dex/minswap_v2_swap.rs`'s tests module:

```rust
#[test]
fn build_update_order_combines_cancel_spend_with_new_swap_output() {
    use crate::requests::types::SwapParams;
    use crate::dex::swap::DexSwap;

    let dex = dex();
    let pool = ada_token_pool(1_000_000_000_000, 5_000_000_000_000, 0.3);

    let sender = "addr1q8n0za95rmvwt45mvxc6lpedh5wx0jl9pyuyfufgzslfdg7tkywsce2nu7xc2pukfsl08x9wlmlnka59c5xcxjamsgfsnyaqug";
    let old_order_address = "addr1z8p79rpkcdz8x9d6tft0x0dx5mwuzac2sa4gm8cvkw5hcn8ftv3526r2y8z8rnlvay76nhpdp9pdekzvd3rrrql08qqssmhdxz";
    let old_order = crate::models::Utxo {
        address: old_order_address.into(),
        tx_hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        tx_index: 0,
        output_index: 0,
        amount: vec![crate::models::Unit { unit: "lovelace".into(), quantity: "5000000".into() }],
        block: String::new(),
        data_hash: None,
        inline_datum: None,
        reference_script_hash: None,
        datum_type: None,
    };

    let params = SwapParams {
        pool: pool.clone(),
        swap_in_token: crate::models::Token::Lovelace,
        swap_in_amount: 100_000_000,
        min_receive: 400_000_000,
        sender_address: sender.into(),
    };

    let pays = dex.build_update_order(&[old_order.clone()], &params).expect("update build");
    assert_eq!(pays.len(), 1);
    let p = &pays[0];

    // New order side: pays to script address with datum
    assert_eq!(p.address, old_order_address);
    assert!(p.datum.is_some());
    assert!(p.assets.iter().any(|a| a.unit == "lovelace"));

    // Cancel side: spend_utxos contains the old order with cancel redeemer + ref script
    assert_eq!(p.spend_utxos.len(), 1);
    let s = &p.spend_utxos[0];
    assert_eq!(s.utxo.tx_hash, old_order.tx_hash);
    assert_eq!(s.redeemer.as_deref(), Some(super::CANCEL_REDEEMER));
    assert!(s.validator_reference.is_some());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib build_update_order`
Expected: compile error — `build_update_order` not on the trait.

- [ ] **Step 3: Add the default impl on `DexSwap`**

In `src/dex/swap.rs`:

```rust
/// Build a single-tx update of an existing order: spend the old order with
/// the cancel redeemer AND create a new order at the same script address
/// with `new_swap_params`. Default impl composes `build_swap_order` and
/// `build_cancel_order`. DEXes whose "update" needs custom redeemer logic
/// can override this method.
fn build_update_order(
    &self,
    old_order_utxos: &[crate::models::Utxo],
    new_swap_params: &crate::requests::types::SwapParams,
) -> anyhow::Result<Vec<crate::requests::types::PayToAddress>> {
    let mut new_order = self.build_swap_order(new_swap_params)?;
    let cancel = self.build_cancel_order(old_order_utxos, &new_swap_params.sender_address)?;
    if new_order.is_empty() {
        anyhow::bail!("build_swap_order returned empty PayToAddress list");
    }
    if cancel.is_empty() || cancel[0].spend_utxos.is_empty() {
        anyhow::bail!("build_cancel_order returned no spend_utxos");
    }
    new_order[0].spend_utxos = cancel[0].spend_utxos.clone();
    Ok(new_order)
}
```

- [ ] **Step 4: Run test to verify pass**

Run: `cargo test --lib build_update_order`
Expected: PASS.

- [ ] **Step 5: Run full test suite**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add src/dex/swap.rs src/dex/minswap_v2_swap.rs
git commit -m "feat(dex): build_update_order default impl on DexSwap trait

Composes build_swap_order + build_cancel_order: takes the new order's
PayToAddress and attaches the old order's spend_utxos (with cancel
redeemer + validator_reference). Single-tx update is then just one
PayToAddress with both output and spend_utxos populated.

Works for any DEX whose 'update' is structurally cancel-old + place-new
in one tx (Minswap V2). DEXes with a custom update redeemer can override."
```

---

### Task 4: Add `UpdateSwapRequest` fluent builder

**Files:**
- Create: `src/requests/update.rs`
- Modify: `src/requests/mod.rs` (re-export)
- Modify: `src/lib.rs` (re-export from top level, matching SwapRequest/CancelSwapRequest pattern)

- [ ] **Step 1: Write the failing test**

Create `tests/update_request.rs`:

```rust
use dexter_kupo_rs::{
    dex::{minswap_v2::MinswapV2, swap::DexSwap},
    models::{Asset, LiquidityPool, Token, Unit, Utxo},
    KupoApi, UpdateSwapRequest,
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

#[test]
fn update_request_builder_returns_combined_paytoaddress() {
    let dex = MinswapV2::new(KupoApi::new("http://localhost:1442"));
    let order_addr = "addr1z8p79rpkcdz8x9d6tft0x0dx5mwuzac2sa4gm8cvkw5hcn8ftv3526r2y8z8rnlvay76nhpdp9pdekzvd3rrrql08qqssmhdxz";
    let sender = "addr1q8n0za95rmvwt45mvxc6lpedh5wx0jl9pyuyfufgzslfdg7tkywsce2nu7xc2pukfsl08x9wlmlnka59c5xcxjamsgfsnyaqug";
    let old_order = Utxo {
        address: order_addr.into(),
        tx_hash: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
        tx_index: 0,
        output_index: 0,
        amount: vec![Unit { unit: "lovelace".into(), quantity: "5000000".into() }],
        block: String::new(),
        data_hash: None,
        inline_datum: None,
        reference_script_hash: None,
        datum_type: None,
    };

    let pays = UpdateSwapRequest::new(&dex)
        .with_old_order_utxos(vec![old_order.clone()])
        .for_pool(pool())
        .with_swap_in_token(Token::Lovelace)
        .with_swap_in_amount(100_000_000)
        .with_minimum_receive(400_000_000)
        .with_sender_address(sender)
        .expect("addr parse")
        .build()
        .expect("update build");

    assert_eq!(pays.len(), 1);
    let p = &pays[0];
    assert_eq!(p.address, order_addr);
    assert!(p.datum.is_some(), "new order datum must be present");
    assert_eq!(p.spend_utxos.len(), 1);
    assert_eq!(p.spend_utxos[0].utxo.tx_hash, old_order.tx_hash);
    assert!(p.spend_utxos[0].redeemer.is_some());
    assert!(p.spend_utxos[0].validator_reference.is_some());
}

#[test]
fn update_request_errors_when_no_old_order_utxos() {
    let dex = MinswapV2::new(KupoApi::new("http://localhost:1442"));
    let err = UpdateSwapRequest::new(&dex)
        .for_pool(pool())
        .with_swap_in_token(Token::Lovelace)
        .with_swap_in_amount(100_000_000)
        .with_minimum_receive(400_000_000)
        .with_sender_address("addr1q8n0za95rmvwt45mvxc6lpedh5wx0jl9pyuyfufgzslfdg7tkywsce2nu7xc2pukfsl08x9wlmlnka59c5xcxjamsgfsnyaqug")
        .expect("addr")
        .build();
    assert!(err.is_err(), "expected error when no old order utxos supplied");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test update_request`
Expected: compile error — `UpdateSwapRequest` doesn't exist.

- [ ] **Step 3: Create `src/requests/update.rs`**

```rust
//! Fluent builder for a single-tx Minswap order update: spends an existing
//! order and places a new one with different parameters, atomically.

use anyhow::{anyhow, Result};

use crate::dex::DexSwap;
use crate::models::Utxo;
use crate::requests::swap::SwapRequest;
use crate::requests::types::PayToAddress;

pub struct UpdateSwapRequest<'a, D: DexSwap + ?Sized> {
    inner: SwapRequest<'a, D>,
    old_order_utxos: Vec<Utxo>,
}

impl<'a, D: DexSwap + ?Sized> UpdateSwapRequest<'a, D> {
    pub fn new(dex: &'a D) -> Self {
        Self { inner: SwapRequest::new(dex), old_order_utxos: vec![] }
    }

    pub fn with_old_order_utxos(mut self, utxos: Vec<Utxo>) -> Self {
        self.old_order_utxos = utxos;
        self
    }

    pub fn for_pool(mut self, pool: crate::models::LiquidityPool) -> Self {
        self.inner = self.inner.for_pool(pool);
        self
    }

    pub fn with_swap_in_token(mut self, token: crate::models::Token) -> Self {
        self.inner = self.inner.with_swap_in_token(token);
        self
    }

    pub fn with_swap_in_amount(mut self, amount: u64) -> Self {
        self.inner = self.inner.with_swap_in_amount(amount);
        self
    }

    pub fn with_slippage_percent(mut self, percent: f64) -> Self {
        self.inner = self.inner.with_slippage_percent(percent);
        self
    }

    pub fn with_minimum_receive(mut self, amount: u64) -> Self {
        self.inner = self.inner.with_minimum_receive(amount);
        self
    }

    pub fn with_sender_address(mut self, addr: &str) -> Result<Self> {
        self.inner = self.inner.with_sender_address(addr)?;
        Ok(self)
    }

    pub fn build(self) -> Result<Vec<PayToAddress>> {
        if self.old_order_utxos.is_empty() {
            return Err(anyhow!("no old order UTxOs supplied for update"));
        }
        // SwapRequest::build() also requires a sender_address — its check will fire if missing.
        // We need access to the dex + swap params; SwapRequest's internals build SwapParams.
        // Easiest path: SwapRequest exposes a method to assemble SwapParams; call it here.
        let (dex, params) = self.inner.into_dex_and_params()?;
        dex.build_update_order(&self.old_order_utxos, &params)
    }
}
```

This requires adding `into_dex_and_params(self) -> Result<(&'a D, SwapParams)>` to `SwapRequest`. If that method doesn't already exist, add it. It should perform the same validation as `build()` (require pool, swap_in_token, swap_in_amount, pricing, sender_address) and return the dex reference + assembled `SwapParams`.

- [ ] **Step 4: Add `into_dex_and_params` to `SwapRequest`**

In `src/requests/swap.rs`, refactor `build` to call a new shared method:

```rust
pub fn into_dex_and_params(self) -> Result<(&'a D, crate::requests::types::SwapParams)> {
    // ... extract pool/swap_in/amount/pricing/sender_address with the same validation as build() ...
    Ok((self.dex, params))
}

pub fn build(self) -> Result<Vec<crate::requests::types::PayToAddress>> {
    let (dex, params) = self.into_dex_and_params()?;
    dex.build_swap_order(&params)
}
```

- [ ] **Step 5: Re-export from `src/requests/mod.rs` and `src/lib.rs`**

`src/requests/mod.rs`:
```rust
pub mod update;
pub use update::UpdateSwapRequest;
```

`src/lib.rs` (top-level re-export, matching `SwapRequest` and `CancelSwapRequest`):
```rust
pub use requests::update::UpdateSwapRequest;
```

- [ ] **Step 6: Run tests**

Run: `cargo test`
Expected: all pass, including the two new ones in `tests/update_request.rs`.

- [ ] **Step 7: Commit**

```bash
git add src/requests/update.rs src/requests/swap.rs src/requests/mod.rs src/lib.rs tests/update_request.rs
git commit -m "feat(requests): UpdateSwapRequest fluent builder

Mirrors SwapRequest + CancelSwapRequest. Wraps SwapRequest internally and
delegates all swap-param methods, adding with_old_order_utxos(). build()
returns a single PayToAddress with both the new order output AND the old
order's spend_utxos (with cancel redeemer + validator_reference), which an
executor can wire into a single atomic tx."
```

---

### Task 5: Update `examples/live_cancel.rs` to prefer reference script when available

**Files:**
- Modify: `examples/live_cancel.rs`

**Why:** Verifies the optimization end-to-end with real on-chain costs. Should drop cancel fee from 0.31 ADA → ~0.20 ADA.

- [ ] **Step 1: Locate the script-attachment site**

Find where `live_cancel.rs` calls `tx.script(ScriptKind::PlutusV2, script_cbor.to_vec())` (inside `build_cancel_tx`).

- [ ] **Step 2: Branch on whether the library returned a reference**

Modify `build_cancel_tx` signature to accept an optional `validator_reference: Option<pallas_txbuilder::Input>` and:
- If Some: call `tx.reference_input(reference_input)` instead of `tx.script(...)`
- If None: call `tx.script(...)` (current behavior, fallback for any DEX with no ref published)

- [ ] **Step 3: In `main`, read the library's `spend.validator_reference` and convert to pallas Input**

```rust
let validator_reference = if let Some(r) = &spend.validator_reference {
    let tx_hash_arr: [u8; 32] = hex::decode(&r.tx_hash)?
        .try_into()
        .map_err(|_| anyhow!("ref tx hash not 32 bytes"))?;
    Some(pallas_txbuilder::Input::new(
        pallas_crypto::hash::Hash::<32>::from(tx_hash_arr),
        r.output_index,
    ))
} else {
    None
};
```

Pass through both build_cancel_tx calls (eval pass + final pass).

- [ ] **Step 4: Build and run dry-run**

Run: `cargo build --example live_cancel 2>&1 | tail -5`
Expected: clean build.

- [ ] **Step 5: Commit**

```bash
git add examples/live_cancel.rs
git commit -m "feat(example): live_cancel uses validator_reference when present

When the library returns a SpendUtxo with validator_reference (Minswap V2
on mainnet does), attach the published script UTxO as a reference_input
instead of carrying the 2659-byte script inline. Tx size drops from ~3.8KB
to ~1KB; cancel fee drops from ~0.31 ADA to ~0.20 ADA. Fallback to inline
script when validator_reference is None (testnet / other DEXes)."
```

---

### Task 6: Add `examples/live_update.rs`

**Files:**
- Create: `examples/live_update.rs`
- Modify: `Cargo.toml` (register example)

**Why:** End-to-end live verification of the new UpdateSwapRequest path. User can run against their own placed order.

- [ ] **Step 1: Create the example**

Layout: mirror `live_cancel.rs` closely. Reads same env vars (`KUPO_URL`, `BLOCKFROST_PROJECT_ID`, `SENDER_BECH32`, `SENDER_MNEMONIC`, `COLLATERAL_UTXO`, plus `POOL_LP_TOKEN_UNIT`, `SWAP_IN_UNIT`, `SWAP_IN_AMOUNT`, `LIMIT_PREMIUM_PERCENT`). CLI flags: `--tx <hash> [--out-index 0] [--submit]`.

Flow:
1. Load env, derive keys, verify SENDER_BECH32 match
2. Look up the old order UTxO via Kupo (`<idx>@<tx>` pattern), validate it's at the script address
3. Look up wallet UTxOs + pinned collateral
4. Fetch pool from Kupo (auto, same as live_swap), recompute min_receive from current price + LIMIT_PREMIUM_PERCENT
5. Build via `UpdateSwapRequest::new(&dex)...build()` → single PayToAddress with both old order's spend_utxos AND new order's address/datum/assets
6. Build pallas tx: collateral_input + (old order as script input with redeemer + validator_reference) + new order as output (inline datum) + change output. Fee + script execution via Blockfrost evaluate. Required signer = payment PKH. Language views from Blockfrost params.
7. Fixpoint fee loop (start at 200_000)
8. Sign, dry-run/submit

- [ ] **Step 2: Register in `Cargo.toml`**

```toml
[[example]]
name = "live_update"
path = "examples/live_update.rs"
```

- [ ] **Step 3: Verify clean build**

Run: `cargo build --example live_update 2>&1 | tail -5`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add examples/live_update.rs Cargo.toml
git commit -m "feat(example): live_update — single-tx order update via UpdateSwapRequest

Mirrors live_cancel structure but spends old order + creates new order in
one atomic tx. Reuses validator_reference for the script (no inline 2659-
byte script). Expected fee: ~0.25 ADA (matches Minswap UI), vs ~0.55 ADA
for separate cancel+place txs."
```

---

### Task 7: Final integration check + user runs `--submit` against a live order

**Files:** none (verification only)

- [ ] **Step 1: Run full test suite**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 2: Run `cargo clippy --all-targets`**

Note any warnings introduced by this branch; fix if non-trivial.

- [ ] **Step 3: Hand off to user**

Print exact commands the user should run:
- `cargo run --example live_swap -- --submit` to place a new test order
- `cargo run --example live_cancel -- --tx <hash>` (dry-run) to verify cancel uses ref script
- `cargo run --example live_update -- --tx <hash>` (dry-run) to verify update structure
- `cargo run --example live_update -- --tx <hash> --submit` to broadcast the live update

User compares fees against the previous separate-tx workflow.

---

## Done criteria

- All 52 existing tests + ~5 new tests pass
- `live_swap.rs` unchanged (still works as before)
- `live_cancel.rs` uses reference script when present; clean build
- `live_update.rs` exists and builds; user verifies live tx on mainnet ≤ 0.3 ADA fee
- Branch ready for finishing-branch flow → merge to master
