# Minswap Order Construction for `dexter-kupo-rs` — Design Spec

**Date:** 2026-06-06
**Status:** Approved design, ready for implementation planning
**Scope:** Add DEX *interaction* (order construction) to the currently read-only `dexter-kupo-rs` crate, starting with Minswap V1 + V2. Port the construction logic from the TypeScript `dexter` library's `requests/` + `dex/` layers.

---

## 1. Background & Motivation

`dexter` (TypeScript, `@indigo-labs/dexter`) can both **read** Cardano DEX data and **interact** with DEXs (build swap orders, cancel orders). It builds an order datum + payment instructions and hands them to a wallet provider (Lucid) that builds/signs/submits the transaction.

`dexter-kupo-rs` (Rust) currently **only reads**: it queries pools/orders via Kupo and decodes Plutus datums with `ciborium`. It has no swap math, no CBOR *encoding*, no order building, and no wallet/tx logic. Its dependencies are lightweight (`reqwest`, `tokio`, `serde`, `anyhow`, `hex`, `ciborium`, `bech32`, …) — notably no Cardano transaction-building library.

This spec adds the **order-construction layer** for Minswap so the crate can be reused by other projects that submit Minswap orders on-chain.

### Key finding: limit orders are net-new (not in dexter)

`dexter` does **not** build Minswap limit orders. The `limitOrderAddress` constant is only used to *detect/cancel* resting orders; the sole builder (`buildSwapOrder`) always emits a `SwapExactIn` order to the **market** address. The limit-order datum was therefore sourced from Minswap's official V2 contract spec
(<https://github.com/minswap/minswap-dex-v2/blob/main/amm-v2-docs/amm-v2-specs.md>).

On-chain reality: a **limit order is the same `SwapExactIn` order step**, with (a) `minimum_receive` derived from a user-chosen price instead of slippage, and (b) for V2, `killable = False` + `expired_setting_opt = None` so the order rests until fillable. dexter already encodes `killable = False`. This means **limit orders fold into the order builder as a parameterization, not a new contract integration.** (Minswap's distinct Stop-Loss and OCO order steps are out of scope.)

---

## 2. Goals & Non-Goals

### Goals (Phase 1)
1. Build **Minswap V1 and V2** swap orders (`SwapExactIn`) → datum CBOR + payment instructions.
2. Build **limit orders** (same `SwapExactIn` datum, user-chosen price; V1 + V2).
3. Build **cancel** instructions for V1 and V2 resting orders (redeemer + order script witness).
4. Provide the **AMM math** (`estimated_receive` / `estimated_give` / `price_impact_percent` / `minimum_receive`).
5. Output the **same boundary object dexter produces** (`PayToAddress`), so a future tx layer can consume it unchanged.
6. Stay **dependency-light** (no Cardano tx-builder) and **pure/unit-testable**.

### Non-Goals (Phase 1 — explicitly out of scope)
- Transaction building, coin selection, fee/change calculation, signing, submission (this is the designed-for **Phase 2** `tx` feature).
- Stop-Loss, OCO, SwapExactOut, zap/deposit/withdraw order types.
- Testnet/preview networks (**mainnet only** for now).
- DEXs other than Minswap V1 / V2.

### Output boundary (decided)
The library's responsibility ends at **order instructions**: it returns a `Vec<PayToAddress>` describing what to pay, where, with which datum (or, for cancel, which UTxO to spend with which redeemer/validator). The consuming project turns that into a transaction. This mirrors exactly where dexter draws the line between its DEX classes and the Lucid wallet provider.

---

## 3. Architecture

### 3.1 Module layout

New code lives alongside the untouched read-only modules:

```
src/
  plutus/
    mod.rs
    data.rs        NEW  PlutusData enum + to_cbor_hex()  (encode counterpart to dex/cbor.rs)
  address.rs       NEW  decode mainnet base addr -> credentials; encode V2 order address
  requests/
    mod.rs         NEW
    types.rs       NEW  PayToAddress, SpendUtxo, PlutusScript, AssetAmount, SwapFee, SwapParams, AddressType, OrderKind
    swap.rs        NEW  SwapRequest builder      -> build() -> Vec<PayToAddress>
    cancel.rs      NEW  CancelSwapRequest builder -> build() -> Vec<PayToAddress>
  dex/
    swap.rs        NEW  DexSwap trait (the "write" trait; reading stays on BaseDex)
    minswap_v1.rs  EXTEND  impl DexSwap (math + order datum + cancel)
    minswap_v2.rs  EXTEND  impl DexSwap (math + order datum + address derivation + cancel)
```

The existing `BaseDex` (reading) trait and all current DEX readers are unchanged.

### 3.2 The boundary object (mirrors dexter's `PayToAddress`)

Swap and cancel share one hand-off shape so the future tx layer has a single thing to consume:

```rust
pub enum AddressType { Contract, Base, Enterprise }

pub enum PlutusVersion { V1, V2 }

pub struct AssetAmount {
    pub unit: String,      // "lovelace" or <policy_id><name_hex>
    pub quantity: u64,
}

pub struct PlutusScript {
    pub version: PlutusVersion,
    pub cbor_hex: String,  // the order validator script (constant per DEX/version)
}

pub struct SpendUtxo {
    pub utxo: Utxo,                     // existing model
    pub redeemer: Option<String>,      // e.g. "d87a80" for cancel
    pub validator: Option<PlutusScript>,
    pub signer: Option<String>,
}

pub struct PayToAddress {
    pub address: String,
    pub address_type: AddressType,
    pub assets: Vec<AssetAmount>,
    pub datum: Option<String>,         // order datum CBOR hex (swap/limit only)
    pub is_inline_datum: bool,         // false for Minswap (datum hash output)
    pub spend_utxos: Vec<SpendUtxo>,   // populated for cancel
}
```

### 3.3 Traits — reading and writing kept separate

```rust
// existing — unchanged
pub trait BaseDex { /* reading: pools, utxos, ... */ }

// new
pub trait DexSwap {
    fn identifier(&self) -> &str;

    fn estimated_receive(&self, pool: &LiquidityPool, in_token: &Token, in_amount: u64) -> u64;
    fn estimated_give(&self, pool: &LiquidityPool, out_token: &Token, out_amount: u64) -> u64;
    fn price_impact_percent(&self, pool: &LiquidityPool, in_token: &Token, in_amount: u64) -> f64;
    fn swap_order_fees(&self) -> Vec<SwapFee>;

    fn build_swap_order(&self, pool: &LiquidityPool, params: &SwapParams) -> anyhow::Result<Vec<PayToAddress>>;
    fn build_cancel_order(&self, order_utxos: &[Utxo], return_address: &str) -> anyhow::Result<Vec<PayToAddress>>;
}
```

`MinswapV1` and `MinswapV2` implement both `BaseDex` (already) and `DexSwap` (new).

---

## 4. Components

### 4.1 `plutus::PlutusData` (Approach C — typed builder)

The one reusable primitive that encodes Plutus data to CBOR, mirroring what Lucid's `C.PlutusData` does under the hood. It is the encode counterpart to the existing decode helpers in `dex/cbor.rs`.

```rust
pub enum PlutusData {
    Constr(u64, Vec<PlutusData>),
    Int(i128),
    Bytes(Vec<u8>),
    List(Vec<PlutusData>),
}

impl PlutusData {
    pub fn bytes_hex(hex: &str) -> anyhow::Result<Self>;   // convenience
    pub fn to_cbor_hex(&self) -> anyhow::Result<String>;   // ciborium encode
}
```

**Constructor tagging rules** (must be implemented generally and unit-tested):
- index `0..=6`  → CBOR tag `121 + index`, value = `Array(fields)`
- index `7..=127` → CBOR tag `1280 + (index - 7)`, value = `Array(fields)`
- index `> 127`  → CBOR tag `102`, value = `Array([Int(index), List(fields)])`

(Minswap uses indices 0 and 1, but the general rule is implemented for correctness and reuse.)

Each DEX datum is then written as explicit, typed Rust that reads like the dexter `order.ts` template — e.g. `Constr(0, vec![Constr(0, vec![PlutusData::bytes_hex(pkh)?]), ...])`.

### 4.2 `address` module (mainnet)

```rust
pub struct WalletAddress {
    pub payment_key_hash: String,        // 28-byte hex
    pub staking_key_hash: Option<String>,// 28-byte hex
    pub bech32: String,
}

// Decode a mainnet base address (addr1...) into its credentials.
pub fn decode_base_address(addr: &str) -> anyhow::Result<WalletAddress>;

// Encode a mainnet base address from a script payment credential + key staking credential.
// Used for the V2 per-user order address (header 0x11 = type 1 base addr, mainnet).
pub fn script_and_stake_to_base_address(script_hash_hex: &str, stake_key_hash_hex: &str) -> anyhow::Result<String>;
```

Reuses the existing `utils::script_hash_to_address` pattern (header-byte + 28-byte payload + `bech32` encode) and its round-trip test style. Decoding: strip header byte, first 28 bytes = payment credential, next 28 = staking credential.

### 4.3 `requests::types::SwapParams`

The resolved parameter bundle the `DexSwap` builders consume (the Rust analogue of dexter's `DatumParameters`):

```rust
pub enum OrderKind { Market, Limit }

pub struct SwapParams {
    pub sender: WalletAddress,
    pub receiver: WalletAddress,        // defaults to sender
    pub swap_in_token: Token,
    pub swap_out_token: Token,
    pub swap_in_amount: u64,
    pub min_receive: u64,               // resolved (from slippage OR limit price OR explicit)
    pub kind: OrderKind,
    pub spend_utxos: Vec<Utxo>,         // optional inputs to consume (dexter's withUtxos)
}
```

### 4.4 `requests::SwapRequest` (ergonomic builder — market + limit unified)

```rust
let pays: Vec<PayToAddress> = SwapRequest::new(&dex)        // &MinswapV1 | &MinswapV2
    .for_pool(pool)
    .with_swap_in_token(token_in)
    .with_swap_in_amount(amount_raw)
    .with_sender_address("addr1...")?                       // mainnet; receiver defaults to sender
    // exactly one pricing mode (last write wins, like dexter):
    .with_slippage_percent(1.0)                             // Market
    .with_limit_price(0.5)                                  // Limit (decimal-aware)
    .with_minimum_receive(1_234_567)                        // Limit (explicit raw)
    .build()?;
```

Read-only helpers mirror dexter: `get_estimated_receive()`, `get_minimum_receive()`, `get_price_impact_percent()`, `get_swap_fees()`. No `.submit()` (that is Phase 2).

**Pricing resolution:**
- **Market** (`with_slippage_percent`): `min_receive = floor(estimated_receive(pool, in, amount) / (1 + slippage/100))`.
- **Limit by price** (`with_limit_price`): decimal-aware
  `min_receive = floor(swap_in_amount × price × 10^(out_decimals − in_decimals))`,
  reading `decimals` off the pool tokens (ADA = 6). `f64` is acceptable — `min_receive` is a user-chosen threshold, not a consensus value. Errors if a token's `decimals` is unknown.
- **Limit by explicit amount** (`with_minimum_receive`): used verbatim.

### 4.5 `requests::CancelSwapRequest`

```rust
let pays = CancelSwapRequest::new(&dex)
    .with_order_utxos(utxos)        // tx outputs to search for the resting order
    .with_return_address("addr1...")
    .build()?;
```

Delegates to `dex.build_cancel_order(...)`.

---

## 5. Per-DEX construction logic

### 5.1 AMM math (shared; identical formulas V1 & V2)

Ported verbatim from `minswap.ts` / `minswap-v2.ts`, parameterized by `pool_fee_percent`. Uses `u128` intermediates to match dexter's `bigint` (no overflow, no float drift). `price_impact_percent` returns the final ratio as `f64`, matching dexter.

- `estimated_receive`: constant-product with fee modifier on a `10_000` basis.
- `estimated_give`: inverse (`+ 1` rounding, as dexter).
- `price_impact_percent`: as dexter.

Fee source: **V1** uses a fixed `0.3%`; **V2** reads `base_fee` from the pool datum (the existing V2 reader already populates `pool_fee_percent`).

### 5.2 Fees (V1 & V2)

`batcher_fee = 2_000000` (not returned) + `deposit = 2_000000` (returned). The order output's lovelace = `batcher + deposit + (swap_in_amount if swap-in token is ADA)`. Native swap-in tokens are added as a separate `AssetAmount`.

### 5.3 Minswap V1 — `build_swap_order` → one `PayToAddress`

- `address` = fixed `marketOrderAddress` `addr1wxn9efv2f6w82hagxqtn62ju4m293tqvw0uhmdl64ch8uwc0h43gt`, `address_type = Contract`.
  - **Limit nuance (to verify):** Minswap's frontend routes limit orders to the base `limitOrderAddress` `addr1zxn9efv2...`. Default Phase-1 behavior emits to `marketOrderAddress` for both modes (batchers index by the shared script hash); routing limit→`limitOrderAddress` is a verification item before release.
- `datum` = V1 order datum (`definitions/minswap/order.ts`) reproduced **exactly** via `PlutusData` (including the template's quirks — e.g. the sender staking slot reusing `SenderPubKeyHash`), then `to_cbor_hex()`. Step is `SwapExactIn { desiredCoin = swap_out (policy,name), minimum_receive }`.
- `assets` per §5.2.

On-chain constants (from `minswap.ts`): `lpTokenPolicyId`, `poolNftPolicyId`, `poolValidityAsset`, `cancelDatum = "d87a80"`, `orderScript` (PlutusV1).

### 5.4 Minswap V2 — `build_swap_order` → one `PayToAddress`

- `address` = **derived per-user**: `script_and_stake_to_base_address(orderScriptHash, sender.staking_key_hash)`
  (`orderScriptHash = c3e28c36c3447315ba5a56f33da6a6ddc1770a876a8d9f0cb3a97c4c`), the Rust equivalent of dexter's `lucidUtils.credentialToAddress(...)`. `address_type = Contract`. Requires the sender address to carry a staking credential.
- `datum` = V2 order datum (`definitions/minswap-v2/order.ts`), 9 fields:
  `canceller, refund_receiver, refund_receiver_datum, success_receiver, success_receiver_datum, lp_asset, step, max_batcher_fee, expired_setting_opt`.
  - `step` = `SwapExactIn { a_to_b_direction, swap_amount_option = SpecificAmount(swap_in_amount), minimum_receive, killable = False }`.
  - `direction` computed by lexicographically sorting in/out token units (exactly as dexter).
  - `lp_asset` from the pool's LP token: policy is constant (`lpTokenPolicyId = f5808c2c...`), name comes from the pool (see §5.6).
  - `max_batcher_fee = batcher_fee`; `expired_setting_opt = None` (`Constr(1, [])`).
- `assets` per §5.2.

### 5.5 Cancel (V1 & V2) → one `PayToAddress` with a `spend_utxos` entry

1. Find the resting order UTxO among the supplied outputs:
   - **V1**: address ∈ { `marketOrderAddress`, `limitOrderAddress` }.
   - **V2**: payment credential of `utxo.address` == `orderScriptHash`.
2. Emit:
   ```
   PayToAddress {
     address: return_address, address_type: Base,
     assets: <order utxo's assets>,
     datum: None, is_inline_datum: false,
     spend_utxos: [ SpendUtxo { utxo, redeemer: "d87a80", validator: <order script>, signer: return_address } ],
   }
   ```
   V1 validator = PlutusV1 `orderScript`; V2 validator = PlutusV2 `orderScript` (constants from the dexter files).

### 5.6 V2 LP-token name source (decided)

`MinswapV2::build_swap_order` reads the pool's LP token name from the `LiquidityPool` passed in. The V2 reader already threads a `pool_id`; it will populate the LP asset **name** so the builder can construct `lp_asset` without an extra lookup. (Confirmed acceptable; no separate LP argument on the request.)

---

## 6. Limit-order semantics (grounded in Minswap V2 spec)

- A limit order is the **same `SwapExactIn` step** as a market swap, distinguished only by how `minimum_receive` is chosen and (V2) the resting flags.
- **V2:** `killable = False` (order rests / is not killed on unmet price) and `expired_setting_opt = None`. dexter already encodes both this way.
- **V1:** `SwapExactIn` has no `killable` flag — it inherently rests until `minimum_receive` is satisfiable or the user cancels. Limit = same datum with a price-derived `minimum_receive`.
- Out of scope: Stop-Loss and OCO (distinct V2 order steps).

Source: Minswap AMM V2 spec — <https://github.com/minswap/minswap-dex-v2/blob/main/amm-v2-docs/amm-v2-specs.md>.

---

## 7. Testing strategy

Correctness here is funds-critical (a wrong datum byte = a stuck/lost order), so testing leads with golden vectors.

### 7.1 Golden-vector tests (mandatory)
- **Market swaps:** generate known-good order datum CBOR from `dexter` (TS) for representative V1 & V2 swaps (ADA→token and token→ADA), assert the Rust `to_cbor_hex()` output is **byte-for-byte identical**.
- **Limit orders:** pull a real on-chain Minswap V1 & V2 limit-order datum (via Kupo/explorer), assert byte-equality of our reconstruction given the same parameters.
- **Cancel:** assert redeemer (`d87a80`), validator script bytes/version, and the spend structure match dexter.

### 7.2 Unit tests
- AMM math (`estimated_receive/give`, `price_impact`) vs dexter outputs across several reserve/amount cases.
- `PlutusData::to_cbor_hex` constructor-tag rules (indices 0, 1, 6, 7, 127, 128) and primitive encodings.
- `decode_base_address` + `script_and_stake_to_base_address` against known mainnet addresses (extend the existing `utils` address tests).
- `with_limit_price` decimal math (in/out decimal combinations; ADA = 6).

---

## 8. Error handling

All fallible builders return `anyhow::Result` (the crate convention). Explicit, non-panicking errors for:
- input token not present in the pool,
- `with_limit_price` used when a token's `decimals` is unknown,
- swap-in amount == 0,
- address that fails to decode or is not a mainnet base address (no staking credential when V2 needs one),
- `estimated_give` that would drain/exceed the out-reserve.

No panics in library code.

---

## 9. Phase 2 (designed-for, not built): the `tx` feature

Because `build()` returns `Vec<PayToAddress>` — the same shape dexter hands to Lucid — Phase 2 is an **additive, feature-gated** module (`tx` Cargo feature, consistent with the crate's existing `export` feature) that consumes that vector: coin-selects over the caller's wallet UTxOs, adds the order output(s) (and, for cancel, the script-witnessed spend), then balances / signs / submits using a Rust Cardano library (e.g. `pallas` or the Rust `cardano-serialization-lib`) plus a submit endpoint. **No Phase-1 API change is required** — the boundary object is the contract between phases. Phase 2 will get its own spec (library choice, key/wallet handling, submit method).

---

## 10. Open verification items (resolve during implementation)

1. **V1 limit address:** confirm whether limit orders must route to `limitOrderAddress` (base) vs `marketOrderAddress` (enterprise), and implement accordingly. Default until verified: market address.
2. **Golden datums:** obtain real on-chain V1 & V2 Minswap limit-order datums to lock the golden vectors.
3. **V2 `killable`/`expired_setting_opt` byte encoding:** confirm `Constr(0, [])` = `False` and `Constr(1, [])` = `None` against a live V2 order datum (consistent with dexter, to be double-checked by golden test).
4. **Decimals availability:** confirm the pool tokens carry `decimals` in the read path when `with_limit_price` is used; otherwise require the caller to pass decimals.
