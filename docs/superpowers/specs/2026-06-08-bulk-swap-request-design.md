# BulkSwapRequest Design

## Goal

Let consumers of `dexter-kupo-rs` produce a single Cardano transaction that
combines:

- N **fresh places** (new orders at the V2 order script address),
- M **cancels** (pure cancel-spends of existing orders, no replacement), and
- U **updates** (cancel an existing order + place a replacement in the same
  transaction — atomic recenter)

in any combination — instead of submitting one tx per intent.

The three kinds are explicit in the bulk builder API rather than auto-detected
from `cancel-then-place` pairs, because callers (e.g., the bot's strategy)
already know whether a cancel is a "recenter" or a "permanent removal" — making
the intent visible avoids surprising fold-in behavior at the SDK layer.

Motivated by the `dexter-mm-bot` ladder strategy, which routinely emits
3-5 actions per tick. Submitting them as separate txs causes UTxO
contention (tx 2 fails because tx 1 consumed the wallet UTxO it wanted)
and pays N × tx-fee + N × ref-script-fee instead of one of each.

## Non-goals

- Cross-DEX bundling. Bundles are scoped to one DEX's pool semantics; this
  spec covers Minswap V2 (the only supported DEX in this lib).
- Optimal bin-packing across multiple txs. If the bundle exceeds Cardano's
  per-tx limits, callers handle splitting at their layer.
- Multi-pool bundling. v1 of the bot is single-pool; the spec admits it
  but the bundle's tx structure is identical regardless of pool count.
- Updating the existing `SwapRequest` / `CancelSwapRequest` /
  `UpdateSwapRequest` APIs. Those keep working unchanged.

## Thesis

The Minswap V2 contract permits arbitrarily many cancel-spends + arbitrarily
many script-output creations in one tx. The published reference script
(CIP-33) is shared across all cancels in the tx — its per-byte fee is paid
**once** regardless of how many cancels reference it. This is where most of
the cost win comes from.

## API

### `BulkOrderPlan` (new public type)

The output is flat from the consumer's perspective: a list of script inputs
(all cancels — both pure-cancel and the cancel-half of updates) and a list
of script outputs (all new orders — both fresh places and the place-half of
updates). The executor stitches them into one tx.

```rust
pub struct BulkOrderPlan {
    /// All script inputs to spend in this tx. Each entry is a Minswap V2 order
    /// UTxO with the cancel redeemer (`d87a80`) + the published script
    /// reference UTxO. Pure cancels and update-cancels both land here — the
    /// tx structure doesn't care which kind they came from.
    ///
    /// Executor responsibility when this is non-empty:
    /// - attach `validator_reference` ONCE (all entries share Minswap's pub ref)
    /// - one redeemer entry per spend_utxo, all `d87a80`
    /// - declare the order owner as a required signer
    /// - provide a collateral input
    pub cancels: Vec<SpendUtxo>,

    /// All new script outputs to create at the V2 order script address. Fresh
    /// places and update-replacements both land here. Each carries its inline
    /// datum and the locked lovelace + (optional) swap-in tokens.
    pub new_orders: Vec<PayToAddress>,

    /// Where to return any change (excess ADA from cancels minus new-order ADA
    /// minus fee) plus any leftover tokens.
    pub return_address: String,
}
```

### Builder — three explicit action kinds

```rust
let plan = BulkSwapRequest::new(&dex)
    .with_return_address(&sender_bech32)?
    .add_cancel(old_order_utxo_a)              // pure cancel: input only
    .add_swap(swap_params_x)                   // pure place: output only
    .add_update(old_order_utxo_b, swap_params_y) // update: input + output, atomic
    .add_swap(swap_params_z)                   // another pure place
    .build()?;                                 // → BulkOrderPlan
```

Builder semantics:
- `add_cancel(utxo: Utxo) -> Self` — append a cancel-only intent. Emits one
  `SpendUtxo` in `cancels`. No corresponding output.
- `add_swap(params: SwapParams) -> Self` — append a fresh place. Emits one
  `PayToAddress` in `new_orders`. No corresponding input.
- `add_update(old_utxo: Utxo, new_params: SwapParams) -> Self` — append an
  atomic update. Emits one `SpendUtxo` in `cancels` AND one `PayToAddress`
  in `new_orders`. The tx doesn't distinguish update-pairs from
  pure-cancel + pure-place — they're structurally identical on-chain — but
  surfacing the intent at the builder layer keeps the strategy's logs and
  intent records honest.
- `build()` validates and produces the `BulkOrderPlan`. Errors when:
  - `return_address` not set
  - All three `add_*` lists are empty (the bundle must have ≥ 1 action)
  - Any swap_params is structurally invalid (same checks as
    `SwapRequest::build`)

### Trait method on `DexSwap`

Default implementation that composes the existing `build_swap_order` and
`build_cancel_order` per item, then aggregates into `BulkOrderPlan`. DEXes
whose bulk semantics differ can override.

```rust
fn build_bulk_orders(
    &self,
    cancels: &[Utxo],                            // pure-cancel inputs
    swaps: &[SwapParams],                        // pure-place params
    updates: &[(Utxo, SwapParams)],              // (old_utxo, new_params) pairs
    return_address: &str,
) -> Result<BulkOrderPlan>;
```

Default impl: for each cancel and each update's old_utxo, call
`build_cancel_order` to get the SpendUtxo (the script-input shape is
identical). For each swap and each update's new_params, call
`build_swap_order` to get the new order PayToAddress. Concatenate into
`BulkOrderPlan`. The "atomic" guarantee of an update is purely a tx-layer
property (everything lands in one tx) — the per-item construction is
unchanged.

## Equivalence to existing APIs

| Old call                                    | Equivalent bulk call                                                          |
|---------------------------------------------|-------------------------------------------------------------------------------|
| `SwapRequest::new(...).build()`             | `BulkSwapRequest::new(...).add_swap(p).build()` → `{ new_orders: [..], cancels: [] }` |
| `CancelSwapRequest::new(...).build()`       | `BulkSwapRequest::new(...).add_cancel(u).build()` → `{ cancels: [..], new_orders: [] }` |
| `UpdateSwapRequest::new(...).build()`       | `BulkSwapRequest::new(...).add_update(old, new).build()` — structurally identical to using add_cancel + add_swap, but the API surfaces the "atomic replace" intent |

The existing three builders are not deprecated. They remain the simpler
API for single-action cases; the bulk builder is for callers managing
many actions.

## Tx-level fee implications

For a bundle with N cancels + M places:
- **Linear fee**: `min_fee_a × tx_size + min_fee_b`. Larger tx → larger linear fee, but linear in size, not in action count. A bundle is much smaller than N+M individual txs because each tx has fixed overhead (~250 bytes of headers, witness scaffolding).
- **Script-execution fee**: N × per-cancel ex_units × `(price_mem, price_step)`. Same as if we submitted N separate cancels — script execution is per-redeemer.
- **Reference-script fee**: ONE × `script_size_bytes × min_fee_ref_script_cost_per_byte`. This is the big win — pays for the 2,659-byte script reference ONCE regardless of N.

Net effect at N=M=2 (typical recenter tick):
- Separate: 4 × ~0.25 ADA = 1.0 ADA
- Bundled: ~0.4-0.5 ADA
- Savings: ~50%

## How callers (executor) use `BulkOrderPlan`

1. **Wallet input selection**: sum `new_orders[i].assets[lovelace]` across i,
   subtract sum of `cancels[i].utxo.amount[lovelace]` (returned from
   cancels), add a fee estimate + min_change. That's the minimum lovelace
   needed from wallet UTxOs.
2. **Token input selection**: any token in any `new_orders[i].assets`
   that isn't ADA must come from wallet UTxOs.
3. **Tx assembly** (pallas):
   - Inputs: selected wallet UTxOs
   - Script inputs: each `cancels[i].utxo` with cancel redeemer
   - Reference input: ONCE, from `cancels[0].validator_reference` (all
     entries share the same one for Minswap V2)
   - Required signers: payment_pkh (from the cancel `signer` field)
   - Outputs: each `new_orders[i]` as a tx output (inline datum)
   - Change output: difference back to `return_address`
   - Collateral input + collateral_return (if any cancels)
   - Language views: PlutusV2 cost model
4. **Ex_units**: Blockfrost evaluate; the response will contain one
   entry per spend redeemer (`spend:0`, `spend:1`, ...). Match by index.
5. **Fee fixpoint loop** (same pattern as live_cancel.rs).

## Testing

| Layer                        | What                                                                                  |
|------------------------------|---------------------------------------------------------------------------------------|
| `BulkSwapRequest` builder    | Unit: build with N places only, M cancels only, U updates only, mixed N+M+U, empty error case |
| `DexSwap::build_bulk_orders` | Unit: bulk-of-1-place == single SwapRequest::build (byte-equal datum)                 |
|                              | Unit: bulk-of-1-cancel == single CancelSwapRequest::build (byte-equal spend_utxos)    |
|                              | Unit: bulk-of-1-update == single UpdateSwapRequest::build (same SpendUtxo + new output) |
| End-to-end                   | One `examples/live_bulk.rs` that places 2 + cancels 1 + updates 1 = 1 tx on mainnet   |

The end-to-end live example is the integration test for tx assembly. The
unit tests verify the library's intermediate types.

## Open questions

1. **Should `add_cancel` validate the UTxO is at the order script address?**
   Yes — same check as `CancelSwapRequest::build`. Bail early.
2. **What if the bundle has 0 cancels and 0 swaps?** `build()` errors with
   "empty bundle" so callers don't silently submit a no-op.
3. **Tx size limit**: Cardano caps at 16 KiB. The Minswap V2 inline datum is
   ~250 bytes; per cancel-spend ~150 bytes. Theoretical max ≈ 60 actions in
   one bundle. Documented limit; callers handle splitting if they hit it.
