# Minswap V2 Order Construction for `dexter-kupo-rs` — Design Spec

**Date:** 2026-06-06
**Status:** Approved design, on-chain-verified, ready for implementation planning
**Scope:** Add DEX *interaction* (order construction) to the currently read-only `dexter-kupo-rs` crate, for **Minswap V2 only**. Port the construction logic from the TypeScript `dexter` library's `requests/` + `dex/minswap-v2.ts` layers.

> **Scope note:** Minswap **V1 is explicitly excluded** — its liquidity is too small to be worth using right now. V1 can be added later by the same pattern if that changes.

---

## 1. Background & Motivation

`dexter` (TypeScript, `@indigo-labs/dexter`) can both **read** Cardano DEX data and **interact** with DEXs (build swap orders, cancel orders). It builds an order datum + payment instructions and hands them to a wallet provider (Lucid) that builds/signs/submits the transaction.

`dexter-kupo-rs` (Rust) currently **only reads**: it queries pools/orders via Kupo and decodes Plutus datums with `ciborium`. It has no swap math, no CBOR *encoding*, no order building, and no wallet/tx logic. Its dependencies are lightweight (`reqwest`, `tokio`, `serde`, `anyhow`, `hex`, `ciborium`, `bech32`, …) — notably no Cardano transaction-building library.

This spec adds the **order-construction layer** for Minswap V2 so the crate can be reused by other projects that submit Minswap orders on-chain.

### Key finding: limit orders are net-new (not in dexter), but structurally identical to swaps

`dexter` does **not** build Minswap limit orders. The sole builder (`buildSwapOrder`) always emits a `SwapExactIn` order. The limit-order datum was sourced from Minswap's official V2 contract spec (<https://github.com/minswap/minswap-dex-v2/blob/main/amm-v2-docs/amm-v2-specs.md>) **and verified against a real on-chain limit order** (see §7.1).

On-chain reality (confirmed): a **limit order is the same `SwapExactIn` step** with (a) `minimum_receive` derived from a user-chosen price instead of slippage, and (b) `killable = False` + `expired_setting_opt = None` so the order rests until fillable. dexter already encodes both flags exactly this way. **Limit orders therefore fold into the order builder as a parameterization, not a new contract integration.** (Minswap's distinct Stop-Loss and OCO order steps are out of scope.)

---

## 2. Goals & Non-Goals

### Goals (Phase 1)
1. Build **Minswap V2** swap orders (`SwapExactIn`) → datum CBOR + payment instructions.
2. Build **limit orders** (same `SwapExactIn` datum, user-chosen price).
3. Build **cancel** instructions for V2 resting orders (redeemer + order script witness).
4. Provide the **AMM math** (`estimated_receive` / `estimated_give` / `price_impact_percent` / `minimum_receive`).
5. Output the **same boundary object dexter produces** (`PayToAddress`), so a future tx layer can consume it unchanged.
6. Stay **dependency-light** (no Cardano tx-builder) and **pure/unit-testable**.

### Non-Goals (Phase 1 — explicitly out of scope)
- **Minswap V1** (low liquidity; not worth it now).
- Transaction building, coin selection, fee/change calculation, signing, submission (the designed-for **Phase 2** `tx` feature).
- Stop-Loss, OCO, SwapExactOut, zap/deposit/withdraw order types.
- Testnet/preview networks (**mainnet only**).
- DEXs other than Minswap V2.

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
    swap.rs        NEW  SwapRequest builder       -> build() -> Vec<PayToAddress>
    cancel.rs      NEW  CancelSwapRequest builder  -> build() -> Vec<PayToAddress>
  dex/
    swap.rs        NEW  DexSwap trait (the "write" trait; reading stays on BaseDex)
    minswap_v2.rs  EXTEND  impl DexSwap (math + order datum + address derivation + cancel)
```

The existing `BaseDex` (reading) trait and all current readers are unchanged. `DexSwap` is kept as a trait (single impl today) so a future DEX/V1 can slot in by the same pattern, mirroring the existing `BaseDex` shape.

### 3.2 The boundary object (mirrors dexter's `PayToAddress`)

Swap and cancel share one hand-off shape so the future tx layer has a single thing to consume:

```rust
pub enum AddressType { Contract, Base, Enterprise }
pub enum PlutusVersion { V1, V2 }

pub struct AssetAmount { pub unit: String, pub quantity: u64 }   // unit = "lovelace" | <policy><name_hex>

pub struct PlutusScript { pub version: PlutusVersion, pub cbor_hex: String }

pub struct SpendUtxo {
    pub utxo: Utxo,                       // existing model
    pub redeemer: Option<String>,         // "d87a80" for cancel
    pub validator: Option<PlutusScript>,  // V2 order script (PlutusV2)
    pub signer: Option<String>,
}

pub struct PayToAddress {
    pub address: String,
    pub address_type: AddressType,
    pub assets: Vec<AssetAmount>,
    pub datum: Option<String>,            // order datum CBOR hex (swap/limit only)
    pub is_inline_datum: bool,            // false — Minswap V2 orders use a datum HASH output (verified)
    pub spend_utxos: Vec<SpendUtxo>,      // populated for cancel
}
```

### 3.3 Traits — reading and writing kept separate

```rust
pub trait BaseDex { /* existing reading API — unchanged */ }

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

`MinswapV2` implements both `BaseDex` (already) and `DexSwap` (new).

---

## 4. Components

### 4.1 `plutus::PlutusData` (Approach C — typed builder)

The reusable primitive that encodes Plutus data to CBOR, mirroring Lucid's `C.PlutusData`. It is the encode counterpart to the decode helpers already in `dex/cbor.rs`.

```rust
pub enum PlutusData { Constr(u64, Vec<PlutusData>), Int(i128), Bytes(Vec<u8>), List(Vec<PlutusData>) }
impl PlutusData {
    pub fn bytes_hex(hex: &str) -> anyhow::Result<Self>;
    pub fn to_cbor_hex(&self) -> anyhow::Result<String>;
}
```

**Constructor tagging rules** (implemented generally, unit-tested):
- index `0..=6`  → CBOR tag `121 + index`, value = `Array(fields)`
- index `7..=127` → CBOR tag `1280 + (index - 7)`
- index `> 127`  → CBOR tag `102`, value = `Array([Int(index), List(fields)])`

> **Verified encoding note:** the real order datum is a definite-length structure but the on-chain bytes use **indefinite-length arrays** inside constructor tags (CBOR `9f … ff`), e.g. `d8799f…ff`. `to_cbor_hex` must reproduce the byte form Minswap/Lucid emit. The golden-vector test (§7.1) is the arbiter of exact bytes; `PlutusData` serialization will be tuned to match it.

Each datum is written as explicit, typed Rust that reads like dexter's `order.ts` template.

### 4.2 `address` module (mainnet) — VERIFIED

```rust
pub struct WalletAddress {
    pub payment_key_hash: String,         // 28-byte hex
    pub staking_key_hash: Option<String>, // 28-byte hex
    pub bech32: String,
}
pub fn decode_base_address(addr: &str) -> anyhow::Result<WalletAddress>;
pub fn script_and_stake_to_base_address(script_hash_hex: &str, stake_key_hash_hex: &str) -> anyhow::Result<String>;
```

Confirmed against the reference tx (§7.1): the V2 order address is a **base address, header `0x11`** (type 1 = script payment credential + key staking credential, network 1 = mainnet), payload = `order_script_hash(28) || sender_stake_key_hash(28)`. Reuses the existing `utils::script_hash_to_address` pattern. Decoding strips the header byte: first 28 bytes = payment credential, next 28 = staking credential.

### 4.3 `requests::types::SwapParams`

```rust
pub enum OrderKind { Market, Limit }
pub struct SwapParams {
    pub sender: WalletAddress,
    pub receiver: WalletAddress,   // defaults to sender
    pub swap_in_token: Token,
    pub swap_out_token: Token,
    pub swap_in_amount: u64,
    pub min_receive: u64,          // resolved (slippage | limit price | explicit)
    pub kind: OrderKind,
    pub kill_on_failed: bool,      // default false (resting); true = fill-or-kill market
    pub spend_utxos: Vec<Utxo>,
}
```

### 4.4 `requests::SwapRequest` (market + limit unified)

```rust
let pays: Vec<PayToAddress> = SwapRequest::new(&dex)        // &MinswapV2
    .for_pool(pool)
    .with_swap_in_token(token_in)
    .with_swap_in_amount(amount_raw)
    .with_sender_address("addr1...")?                       // mainnet base addr; receiver defaults to sender
    // exactly one pricing mode (last write wins, like dexter):
    .with_slippage_percent(1.0)                             // Market
    .with_limit_price(0.5)                                  // Limit (decimal-aware)
    .with_minimum_receive(1_234_567)                        // Limit (explicit raw)
    .build()?;
```

Read-only helpers mirror dexter: `get_estimated_receive()`, `get_minimum_receive()`, `get_price_impact_percent()`, `get_swap_fees()`. No `.submit()` (Phase 2).

**Pricing resolution:**
- **Market** (`with_slippage_percent`): `min_receive = floor(estimated_receive(pool, in, amount) / (1 + slippage/100))`.
- **Limit by price** (`with_limit_price`): decimal-aware
  `min_receive = floor(swap_in_amount × price × 10^(out_decimals − in_decimals))`, reading `decimals` off the pool tokens (ADA = 6). `f64` acceptable — `min_receive` is a user threshold, not a consensus value. **Errors** if a token's `decimals` is unknown (see §8).
- **Limit by explicit amount** (`with_minimum_receive`): used verbatim.

`kill_on_failed` defaults to `false` (resting), matching dexter and the verified on-chain order; an optional setter exposes fill-or-kill market behavior.

### 4.5 `requests::CancelSwapRequest`

```rust
let pays = CancelSwapRequest::new(&dex)
    .with_order_utxos(utxos)          // tx outputs to search for the resting order
    .with_return_address("addr1...")
    .build()?;
```

Delegates to `dex.build_cancel_order(...)`.

---

## 5. Minswap V2 construction logic

### 5.1 On-chain constants (from `minswap-v2.ts`, script hash re-verified on-chain)
- `lpTokenPolicyId   = f5808c2c990d86da54bfc97d89cee6efa20cd8461616359478d96b4c`
- `poolValidityAsset = f5808c2c990d86da54bfc97d89cee6efa20cd8461616359478d96b4c4d5350`
- `orderScriptHash   = c3e28c36c3447315ba5a56f33da6a6ddc1770a876a8d9f0cb3a97c4c`  *(confirmed identical to the live order address payment credential)*
- `cancelDatum (redeemer) = d87a80`
- `orderScript` = PlutusV2 script (constant from `minswap-v2.ts`)

### 5.2 AMM math (ported from `minswap-v2.ts`)
`u128` intermediates (match dexter's `bigint`); `10_000` fee basis; `pool_fee_percent` from the pool's `base_fee` (the existing V2 reader already populates it). `price_impact_percent` returns the final ratio as `f64`.
- `estimated_receive`, `estimated_give` (`+1` rounding as dexter), `price_impact_percent`.

### 5.3 Fees
`max_batcher_fee = 2_000000` (verified `Int 2000000` in the datum) + `deposit = 2_000000` (min-UTxO, returned). Order output lovelace = `batcher + deposit + (swap_in_amount if swap-in is ADA)` — verified `2_004_000000 = 2_000_000000 swap-in + 2 batcher + 2 deposit`. Native swap-in tokens are added as a separate `AssetAmount`.

### 5.4 `build_swap_order` → one `PayToAddress`
- `address` = `script_and_stake_to_base_address(orderScriptHash, sender.staking_key_hash)` → base addr `addr1z…` (verified). `address_type = Contract`. **Requires** the sender address to carry a staking credential.
- `datum` = V2 order datum (9 fields, see §6), built with `PlutusData` and `to_cbor_hex()`.
- `assets` per §5.3.
- `is_inline_datum = false` (datum-hash output, verified).

### 5.5 `build_cancel_order` → one `PayToAddress` with a `spend_utxos` entry
1. Find the resting order UTxO whose `utxo.address` payment credential == `orderScriptHash`.
2. Emit:
   ```
   PayToAddress {
     address: return_address, address_type: Base,
     assets: <order utxo's assets>, datum: None, is_inline_datum: false,
     spend_utxos: [ SpendUtxo { utxo, redeemer: "d87a80", validator: <PlutusV2 orderScript>, signer: return_address } ],
   }
   ```

### 5.6 V2 LP-token name source (decided)
`build_swap_order` reads the pool's LP token name from the `LiquidityPool` passed in (LP policy is the constant; LP name is per-pool — verified 32-byte name `7dd6988c…` in field 5). The V2 reader populates `LiquidityPool` with the LP asset name so the builder constructs `lp_asset` without an extra lookup.

---

## 6. V2 `OrderDatum` layout (verified field-by-field)

9-field `Constr 0`, matching dexter's `definitions/minswap-v2/order.ts` exactly:

| # | Field | Encoding | Source |
|---|---|---|---|
| 0 | `canceller` | `Constr0[ Bytes(sender_payment_key_hash) ]` | sender addr |
| 1 | `refund_receiver` | `Constr0[ Constr0[Bytes(pkh)], Constr0[Constr0[Constr0[Bytes(stake)]]] ]` | sender addr |
| 2 | `refund_receiver_datum` | `Constr0[]` | constant |
| 3 | `success_receiver` | same shape as field 1 | receiver (= sender) |
| 4 | `success_receiver_datum` | `Constr0[]` | constant |
| 5 | `lp_asset` | `Constr0[ Bytes(lpTokenPolicyId), Bytes(lp_name) ]` | constant + pool |
| 6 | `step` (SwapExactIn) | `Constr0[ Constr(direction)[], Constr0[Int(swap_in_amount)], Int(min_receive), Constr0[] ]` | request |
| 7 | `max_batcher_fee` | `Int(2_000000)` | constant |
| 8 | `expired_setting_opt` | `Constr1[]` (None) | constant |

Where in field 6: `direction` = `1` if `swap_in_unit` sorts before `swap_out_unit` lexicographically else `0` (ADA = empty string sorts first → ADA-in gives `direction = 1`, verified); the trailing `Constr0[]` is `killable = False` (resting). For a fill-or-kill market order, `killable` = `Constr1[]` (`True`) and `min_receive` comes from slippage.

---

## 7. Testing strategy

### 7.1 Golden vector (verified, mandatory)
Primary reference — a real on-chain V2 limit order placed by the project owner:
- **Tx:** `ddabf4cd690979b4c19a3c46117d3837966f35c0e1db8b59fd972cb7d8e3fca3` (mainnet)
- **Order output #1 datum hash:** `6128a1cb53233958ea9294c636d1aad066c2da141ac3d58190295a71f0e74fb5`
- **Datum CBOR (golden):**
  `d8799fd8799f581ce6f174b44628064bfdc62cdf2be63c50b2cd68625efe6847f5fdbf83ffd8799fd8799f581ce6f174b44628064bfdc62cdf2be63c50b2cd68625efe6847f5fdbf83ffd8799fd8799fd8799f581c43f42529ba296fb3fbbbd14817897731219a5339d35195aa6e7d6b22ffffffffd87980d8799fd8799f581ce6f174b44628064bfdc62cdf2be63c50b2cd68625efe6847f5fdbf83ffd8799fd8799fd8799f581c43f42529ba296fb3fbbbd14817897731219a5339d35195aa6e7d6b22ffffffffd87980d8799f581cf5808c2c990d86da54bfc97d89cee6efa20cd8461616359478d96b4c58207dd6988c5a86693c76aeec1ea94afa41770be0de21a775ca7a2a1eabdb6a0171ffd8799fd87a80d8799f1a77359400ff1a1460ae15d87980ff1a001e8480d87a80ff`
- **Decoded params:** sender pkh `e6f174b4…`, stake `43f42529…`, LP `f5808c2c… / 7dd6988c…`, direction `1`, swap-in `2_000_000000`, min_receive `341_880_341`, batcher `2_000000`, killable `False`, expiry `None`.

The `build_swap_order` test reconstructs this datum from the decoded params and asserts `to_cbor_hex()` == the golden CBOR **byte-for-byte**. This single test pins the datum structure, the constructor tags, the indefinite-array byte form, and the killable/expiry flags.

### 7.2 Supplementary golden vectors (optional)
Capture additional market-swap datums from `dexter` (TS) — token→ADA and token→token — to cover `direction = 0` and native-token swap-in. Lower priority now that the structure is byte-confirmed; requires standing up the TS toolchain.

### 7.3 Unit tests
- AMM math (`estimated_receive/give`, `price_impact`) vs dexter outputs across several reserve/amount cases.
- `PlutusData::to_cbor_hex` tag rules (indices 0,1,6,7,127,128) and primitives.
- `decode_base_address` + `script_and_stake_to_base_address` against the verified addresses in §7.1 (order `addr1z8p79rpk…`, sender `addr1qyfd4vf3…`).
- `with_limit_price` decimal math (in/out decimal combinations; ADA = 6).

---

## 8. Error handling
All fallible builders return `anyhow::Result` (crate convention). Explicit, non-panicking errors for: input token not in pool; `with_limit_price` used when a token's `decimals` is unknown; swap-in amount == 0; address that fails to decode / is not a mainnet base address / lacks a staking credential; `estimated_give` exceeding the out-reserve. No panics in library code.

---

## 9. Phase 2 (designed-for, not built): the `tx` feature
Because `build()` returns `Vec<PayToAddress>` — the same shape dexter hands to Lucid — Phase 2 is an **additive, feature-gated** module (`tx` Cargo feature, consistent with the existing `export` feature) that consumes that vector: coin-selects over the caller's wallet UTxOs, adds the order output(s) (and, for cancel, the script-witnessed spend + collateral), then balances / signs / submits using a Rust Cardano library (e.g. `pallas` or the Rust `cardano-serialization-lib`) plus a submit endpoint. **No Phase-1 API change is required.** Phase 2 gets its own spec (library choice, key/wallet handling, submit method).

---

## 10. Verification status

| Item | Status |
|---|---|
| V2 order address derivation (header `0x11`, script hash + sender stake) | ✅ Verified on-chain (§7.1) |
| Order script hash matches dexter constant | ✅ Verified (`c3e28c36…`) |
| V2 limit-order datum == dexter `SwapExactIn` template | ✅ Verified byte-for-byte (§6, §7.1) |
| `killable = Constr0[] = False`, `expired = Constr1[] = None` | ✅ Verified |
| `max_batcher_fee = 2_000000`; output lovelace = swap-in + 4 ADA | ✅ Verified |
| Datum is a hash output (not inline) | ✅ Verified |
| `direction` lexicographic rule (ADA-in → 1) | ✅ Verified |
| Token `decimals` available when `with_limit_price` is used | ⚠️ Open — guarded by an explicit error (§8); confirm read path populates decimals, else require caller-supplied decimals |
| `PlutusData::to_cbor_hex` reproduces indefinite-array byte form | ⚠️ Implementation detail — pinned by the §7.1 golden test |
