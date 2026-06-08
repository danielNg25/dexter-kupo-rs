//! Live Minswap V2 BULK example — builds + signs + (optionally) submits a
//! bundled transaction containing any combination of:
//!   - Pure cancels   (--cancel <tx_hash>#<idx>, repeatable)
//!   - Pure places    (--n-swaps <n>  fresh orders using env-var swap params)
//!   - Atomic updates (--update <tx_hash>#<idx>, repeatable)
//!
//! USAGE:
//!   cargo run --example live_bulk -- \
//!     [--cancel <tx_hash>#<idx>]   # repeatable: pure cancel
//!     [--update <tx_hash>#<idx>]   # repeatable: cancel-half of update
//!     [--n-swaps <count>]          # fresh swap places (default 0)
//!     [--submit]
//!
//! All fresh swaps and update place-halves share the same swap params from env
//! (SWAP_IN_UNIT, SWAP_IN_AMOUNT, LIMIT_PREMIUM_PERCENT).
//!
//! ENV VARS (superset of live_update.rs):
//!   KUPO_URL                   local Kupo API URL (http://localhost:1442)
//!   BLOCKFROST_PROJECT_ID      mainnet Blockfrost project ID (mainnet...)
//!   SENDER_BECH32              sender mainnet base address (addr1...)
//!   SENDER_MNEMONIC            BIP-39 mnemonic phrase
//!   POOL_LP_TOKEN_UNIT         V2 pool LP token unit
//!   SWAP_IN_UNIT               "lovelace" or <policy_hex><name_hex>
//!   SWAP_IN_AMOUNT             raw u64 amount to swap in
//!
//! OPTIONAL:
//!   SENDER_MNEMONIC_PASSPHRASE  BIP-39 passphrase (default: empty)
//!   LIMIT_PREMIUM_PERCENT       min_receive = estimate * (1 + X/100); default 10.0
//!   COLLATERAL_UTXO             pin a wallet UTxO `<tx_hash>#<idx>` as collateral
//!
//! KEY COST WIN:
//!   The reference-script fee is charged ONCE regardless of how many cancels
//!   reference it. A bundle of N cancels pays one ref-script fee instead of N.
//!   At typical bot scale (4 actions/tick) this saves ~50% per tx cycle.
//!
//! BALANCE MODEL:
//!   - Script inputs: plan.cancels  (each with cancel redeemer, shared ref script)
//!   - Wallet inputs: coin-selected pure-ADA UTxOs covering sum(new_order ADA) + fee
//!   - Script outputs: plan.new_orders (inline datum, exact ADA the library specified)
//!   - Change output: leftover ADA back to sender
//!   - Reference input: ONE (shared for all cancels)
//!   - Collateral: COLLATERAL_UTXO (pinned; excluded from coin selection)
//!
//! # Safety
//!   Default mode is DRY-RUN. Pass --submit to broadcast. Real ADA spent.

use anyhow::{anyhow, bail, Context, Result};
use bip39::{Language, Mnemonic};
use dexter_kupo_rs::{
    BulkSwapRequest,
    dex::{minswap_v2::MinswapV2, BaseDex, DexSwap},
    models::{token_identifier, Asset, Token},
    KupoApi,
};
use ed25519_bip32::{DerivationScheme, XPrv};
use pallas_addresses::Address as PallasAddress;
use pallas_crypto::{hash::Hash, key::ed25519};
use pallas_primitives::conway::LanguageViews;
use pallas_txbuilder::{BuildConway, ExUnits, Input, Output, ScriptKind, StagingTransaction};
use std::{collections::BTreeMap, env};

const BLOCKFROST_BASE: &str = "https://cardano-mainnet.blockfrost.io/api/v0";
const CARDANOSCAN_TX: &str = "https://cardanoscan.io/transaction";
const HARDENED: u32 = 0x8000_0000;

// ---------- parsed CLI action ----------

/// A cancel or update-cancel-half: the order UTxO identified by tx_hash + output index.
struct OrderRef {
    tx_hash: String,
    output_index: u64,
}

// ---------- main ----------

#[tokio::main]
async fn main() -> Result<()> {
    match dotenvy::dotenv() {
        Ok(p) => eprintln!(".env loaded from {}", p.display()),
        Err(dotenvy::Error::Io(ref e)) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(dotenvy::Error::LineParse(_, idx)) => {
            eprintln!(
                ".env parse error at line offset {}. \
                 Common cause: a value with spaces is not quoted. \
                 Wrap SENDER_MNEMONIC in double quotes. \
                 (Line content suppressed to avoid leaking secrets.)",
                idx
            );
        }
        Err(e) => eprintln!(".env error: {:?} (details suppressed)", std::mem::discriminant(&e)),
    }

    let args: Vec<String> = env::args().collect();
    let submit = args.iter().any(|a| a == "--submit");

    // --cancel <tx_hash>#<idx>  (repeatable)
    let cancel_refs: Vec<OrderRef> = collect_flag_values(&args, "--cancel")
        .into_iter()
        .map(|s| parse_utxo_ref(&s).with_context(|| format!("parsing --cancel {}", s)))
        .collect::<Result<Vec<_>>>()?;

    // --update <tx_hash>#<idx>  (repeatable)
    let update_refs: Vec<OrderRef> = collect_flag_values(&args, "--update")
        .into_iter()
        .map(|s| parse_utxo_ref(&s).with_context(|| format!("parsing --update {}", s)))
        .collect::<Result<Vec<_>>>()?;

    // --n-swaps <count>  (default 0)
    let n_swaps: usize = collect_flag_values(&args, "--n-swaps")
        .into_iter()
        .next()
        .map(|s| s.parse::<usize>().context("--n-swaps must be a non-negative integer"))
        .transpose()?
        .unwrap_or(0);

    if cancel_refs.is_empty() && update_refs.is_empty() && n_swaps == 0 {
        bail!(
            "no actions specified. Use --cancel, --update, or --n-swaps to build a bundle.\n\
             Example: cargo run --example live_bulk -- --n-swaps 1 --cancel <tx>#0"
        );
    }

    // Env config — requires pool vars only when there are swaps/updates.
    let needs_swap_params = n_swaps > 0 || !update_refs.is_empty();
    let cfg = Config::from_env(needs_swap_params).context("loading env vars")?;

    // ── Key derivation ─────────────────────────────────────────────────────────
    let (payment_xprv, _stake_xprv, derived_addr) =
        derive_account_keys(&cfg.sender_mnemonic, &cfg.sender_mnemonic_passphrase)
            .context("deriving keys from mnemonic")?;
    if derived_addr != cfg.sender_bech32 {
        bail!(
            "mnemonic does not match SENDER_BECH32.\n  derived:  {}\n  expected: {}",
            derived_addr, cfg.sender_bech32
        );
    }
    eprintln!("✓ derived address matches SENDER_BECH32");

    let ext_bytes: [u8; 64] = payment_xprv.extended_secret_key();
    let signing_key = ed25519::SecretKeyExtended::from_bytes(ext_bytes)
        .map_err(|_| anyhow!("building signing key"))?;
    let _ = ext_bytes;

    let payment_pub: [u8; 32] = payment_xprv.public().public_key();
    let payment_pkh: [u8; 28] = {
        let h = pallas_crypto::hash::Hasher::<224>::hash(&payment_pub);
        let mut out = [0u8; 28];
        out.copy_from_slice(h.as_ref());
        out
    };

    let client = reqwest::Client::new();

    eprintln!("== Live Bulk (Minswap V2) ==");
    eprintln!("mode:     {}", if submit { "SUBMIT — real ADA will be spent" } else { "DRY-RUN (pass --submit to broadcast)" });
    eprintln!("sender:   {}", cfg.sender_bech32);
    eprintln!("cancels:  {}", cancel_refs.len());
    eprintln!("updates:  {}", update_refs.len());
    eprintln!("n-swaps:  {}", n_swaps);
    eprintln!();

    let kupo = KupoApi::new(&cfg.kupo_url);
    let dex = MinswapV2::new(KupoApi::new(&cfg.kupo_url));

    // 1. Resolve cancel/update order UTxOs from Kupo.
    let mut cancel_utxos = Vec::new();
    for r in &cancel_refs {
        let pattern = format!("{}@{}", r.output_index, r.tx_hash);
        let utxos = kupo.get(&pattern, true).await
            .with_context(|| format!("fetching cancel UTxO {} from Kupo", pattern))?;
        if utxos.is_empty() {
            bail!(
                "cancel UTxO {} not found at Kupo. Either (a) already spent (filled/cancelled), \
                 or (b) Kupo is not indexing the order script address.",
                pattern
            );
        }
        eprintln!("✓ cancel UTxO found: {} ({})", pattern, utxos[0].address);
        cancel_utxos.push(utxos[0].clone());
    }

    let mut update_utxos = Vec::new();
    for r in &update_refs {
        let pattern = format!("{}@{}", r.output_index, r.tx_hash);
        let utxos = kupo.get(&pattern, true).await
            .with_context(|| format!("fetching update UTxO {} from Kupo", pattern))?;
        if utxos.is_empty() {
            bail!(
                "update UTxO {} not found at Kupo. Either (a) already spent, \
                 or (b) Kupo is not indexing the order script address.",
                pattern
            );
        }
        eprintln!("✓ update UTxO found: {} ({})", pattern, utxos[0].address);
        update_utxos.push(utxos[0].clone());
    }

    // 2. Fetch pool (only needed if there are swaps or updates).
    let (pool, swap_in, min_receive) = if needs_swap_params {
        let pool_lp_unit = cfg.pool_lp_token_unit.as_deref()
            .ok_or_else(|| anyhow!("POOL_LP_TOKEN_UNIT required for swaps/updates"))?;
        let swap_in_unit = cfg.swap_in_unit.as_deref()
            .ok_or_else(|| anyhow!("SWAP_IN_UNIT required for swaps/updates"))?;
        let swap_in_amount = cfg.swap_in_amount
            .ok_or_else(|| anyhow!("SWAP_IN_AMOUNT required for swaps/updates"))?;

        let pool = dex.liquidity_pool_from_pool_id(pool_lp_unit).await
            .context("fetching pool from Kupo (is Kupo indexing the V2 validity asset?)")?
            .ok_or_else(|| anyhow!(
                "no pool found for LP token unit `{}`. Verify (a) the unit is correct and \
                 (b) Kupo is indexing the Minswap V2 validity asset.",
                pool_lp_unit
            ))?;

        eprintln!("pool fetched from Kupo:");
        eprintln!("  assets:   {} / {}", token_unit(&pool.asset_a), token_unit(&pool.asset_b));
        eprintln!("  reserves: {} / {}", pool.reserve_a, pool.reserve_b);
        eprintln!("  fee:      {}%", pool.pool_fee_percent);

        let swap_in_tok = token_from_unit(swap_in_unit)?;
        let in_id = token_identifier(&swap_in_tok);
        let a_id = token_identifier(&pool.asset_a);
        let b_id = token_identifier(&pool.asset_b);
        if in_id != a_id && in_id != b_id {
            bail!(
                "SWAP_IN_UNIT `{}` is not in the pool. Pool assets: `{}` / `{}`.",
                swap_in_unit, a_id, b_id
            );
        }

        let estimated = dex.estimated_receive(&pool, &swap_in_tok, swap_in_amount);
        let multiplier = 1.0 + cfg.limit_premium_percent / 100.0;
        let mr = (estimated as f64 * multiplier).floor() as u64;

        eprintln!();
        eprintln!("swap params (shared for all new orders):");
        eprintln!("  in:           {} of {}", swap_in_amount, swap_in_unit);
        eprintln!("  estimated:    {} raw out", estimated);
        eprintln!("  limit premium: +{}%", cfg.limit_premium_percent);
        eprintln!("  min_receive:  {}", mr);
        eprintln!();

        (Some(pool), Some(swap_in_tok), Some(mr))
    } else {
        (None, None, None)
    };

    // 3. Build BulkOrderPlan via BulkSwapRequest.
    let sender_addr_str = cfg.sender_bech32.clone();
    let mut builder = BulkSwapRequest::new(&dex)
        .with_return_address(&sender_addr_str)?;

    if let Some(ref p) = pool {
        builder = builder.for_pool(p.clone());
    }

    for utxo in cancel_utxos {
        builder = builder.add_cancel(utxo);
    }

    // Build swap params (all new orders share the same params in this example).
    if n_swaps > 0 || !update_utxos.is_empty() {
        let swap_in_unit_str = cfg.swap_in_unit.as_deref().unwrap_or("lovelace");
        let swap_in_amount = cfg.swap_in_amount.unwrap_or(0);
        let mr = min_receive.unwrap_or(0);
        let swap_in_tok = swap_in.as_ref().unwrap();

        use dexter_kupo_rs::address::decode_base_address;
        use dexter_kupo_rs::requests::types::{OrderKind, SwapParams};
        let decoded_addr = decode_base_address(&cfg.sender_bech32)
            .with_context(|| format!("decoding sender address `{}`", cfg.sender_bech32))?;

        let swap_out_tok = if token_identifier(swap_in_tok) == token_identifier(&pool.as_ref().unwrap().asset_a) {
            pool.as_ref().unwrap().asset_b.clone()
        } else {
            pool.as_ref().unwrap().asset_a.clone()
        };

        let base_params = SwapParams {
            sender: decoded_addr.clone(),
            receiver: decoded_addr,
            swap_in_token: swap_in_tok.clone(),
            swap_out_token: swap_out_tok,
            swap_in_amount,
            min_receive: mr,
            kind: OrderKind::Limit,
            kill_on_failed: false,
            spend_utxos: vec![],
        };

        for _ in 0..n_swaps {
            builder = builder.add_swap(base_params.clone());
        }

        for utxo in update_utxos {
            builder = builder.add_update(utxo, base_params.clone());
        }

        let _ = swap_in_unit_str; // used for logging above
    }

    let plan = builder.build().context("building BulkOrderPlan")?;

    // CANONICAL CANCEL ORDER:
    // pallas-txbuilder sorts tx body inputs by (tx_hash, output_index) ascending
    // when serializing. Blockfrost evaluate returns `spend:N` indexed by that
    // sorted position — NOT the order the inputs were added in. To keep
    // redeemer→ex_units mapping correct, sort the cancels once here and use
    // `sorted_cancels` everywhere downstream (input construction, evaluate
    // result indexing, balance math).
    let mut sorted_cancels = plan.cancels.clone();
    sorted_cancels.sort_by(|a, b| {
        a.utxo.tx_hash.cmp(&b.utxo.tx_hash)
            .then(a.utxo.output_index.cmp(&b.utxo.output_index))
    });

    eprintln!("BulkOrderPlan:");
    eprintln!("  cancels:    {}", sorted_cancels.len());
    eprintln!("  new_orders: {}", plan.new_orders.len());

    // 4. Resolve script details from the first cancel (all share the same validator).
    //    If there are no cancels, this is a pure-place tx and we skip script setup.
    //    Iterate sorted_cancels so redeemer order matches the tx body input order.
    let (script_cbor, validator_reference, redeemers_cbor, ref_script_size) =
        if sorted_cancels.is_empty() {
            (vec![], None, vec![], 0u64)
        } else {
            let first_spend = &sorted_cancels[0];
            let sc = hex::decode(
                &first_spend.validator.as_ref()
                    .ok_or_else(|| anyhow!("cancel[0]: library missing validator"))?
                    .cbor_hex
            ).context("decoding script cbor")?;

            let vref: Option<Input> = first_spend.validator_reference
                .as_ref()
                .map(|r| -> Result<_> {
                    Ok(Input::new(parse_hash32(&r.tx_hash)?, r.output_index))
                })
                .transpose()?;
            let ref_size = first_spend.validator_reference
                .as_ref()
                .map(|r| r.script_size_bytes)
                .unwrap_or(0);

            match &first_spend.validator_reference {
                Some(r) => eprintln!(
                    "✓ using reference script UTxO {}#{} (saves ~2.6KB; paid ONCE for all {} cancel(s))",
                    r.tx_hash, r.output_index, sorted_cancels.len()
                ),
                None => eprintln!("warning: no validator_reference; using inline script (2.6KB in witness set)"),
            }

            // Collect redeemer CBOR for every cancel input, in sorted order.
            let mut redms: Vec<Vec<u8>> = Vec::new();
            for (i, spend) in sorted_cancels.iter().enumerate() {
                let r = hex::decode(
                    spend.redeemer.as_ref()
                        .ok_or_else(|| anyhow!("cancel[{}]: missing redeemer", i))?
                ).with_context(|| format!("decoding redeemer cbor for cancel[{}]", i))?;
                redms.push(r);
            }

            (sc, vref, redms, ref_size)
        };

    // 5. Collateral UTxO.
    const MIN_COLLATERAL_LOVELACE: u64 = 2_000_000;
    let has_script = !sorted_cancels.is_empty();
    let collateral = if has_script {
        if let Some((c_tx, c_idx)) = &cfg.collateral_utxo {
            let pattern = format!("{}@{}", c_idx, c_tx);
            let utxos = kupo.get(&pattern, true).await
                .with_context(|| format!("fetching pinned collateral {} from Kupo", pattern))?;
            let u = utxos.into_iter().next()
                .ok_or_else(|| anyhow!(
                    "COLLATERAL_UTXO {} not found at Kupo (either spent or not indexed). \
                     If you just sent funds, wait for confirmation.",
                    pattern
                ))?;
            if u.address != cfg.sender_bech32 {
                bail!(
                    "COLLATERAL_UTXO {} is at address {}, not SENDER_BECH32 {}",
                    pattern, u.address, cfg.sender_bech32
                );
            }
            if u.amount.len() != 1 || u.amount[0].unit != "lovelace" {
                bail!("COLLATERAL_UTXO {} must be pure ADA, but holds: {:?}", pattern, u.amount);
            }
            let qty: u64 = u.amount[0].quantity.parse()
                .map_err(|_| anyhow!("COLLATERAL_UTXO quantity not u64"))?;
            if qty < MIN_COLLATERAL_LOVELACE {
                bail!(
                    "COLLATERAL_UTXO {} has only {} lovelace, need at least {}",
                    pattern, qty, MIN_COLLATERAL_LOVELACE
                );
            }
            eprintln!("✓ using pinned COLLATERAL_UTXO {}", pattern);
            Some((u, qty))
        } else {
            let wallet_utxos = kupo.get(&cfg.sender_bech32, true).await
                .context("fetching wallet UTxOs from Kupo")?;
            let best = wallet_utxos.iter()
                .filter(|u| u.amount.len() == 1 && u.amount[0].unit == "lovelace")
                .filter_map(|u| {
                    let qty: u64 = u.amount[0].quantity.parse().ok()?;
                    (qty >= MIN_COLLATERAL_LOVELACE).then_some((u.clone(), qty))
                })
                .max_by_key(|(_, q)| *q)
                .ok_or_else(|| anyhow!(
                    "no pure-ADA UTxO >= {} lovelace in wallet for collateral. \
                     Either top up the wallet, or set COLLATERAL_UTXO=<tx_hash>#<idx>.",
                    MIN_COLLATERAL_LOVELACE
                ))?;
            eprintln!("✓ auto-collateral: {}@{} ({} lovelace)",
                best.0.output_index, best.0.tx_hash, best.1);
            Some(best)
        }
    } else {
        None
    };
    if let Some(ref c) = collateral {
        eprintln!("✓ collateral: {}@{} ({} lovelace)", c.0.output_index, c.0.tx_hash, c.1);
    }

    // 6. Fetch protocol params.
    let params = blockfrost_get(&client, &cfg.bf_project_id, "/epochs/latest/parameters")
        .await
        .context("fetching protocol params")?;
    let min_fee_a = params["min_fee_a"].as_u64()
        .ok_or_else(|| anyhow!("min_fee_a missing"))?;
    let min_fee_b = params["min_fee_b"].as_u64()
        .ok_or_else(|| anyhow!("min_fee_b missing"))?;
    let price_mem = parse_price(&params["price_mem"]).context("parsing price_mem")?;
    let price_step = parse_price(&params["price_step"]).context("parsing price_step")?;
    let collateral_percent = params["collateral_percent"].as_u64().unwrap_or(150);
    let ref_script_cost_per_byte: f64 = params["min_fee_ref_script_cost_per_byte"]
        .as_f64()
        .or_else(|| params["min_fee_ref_script_cost_per_byte"].as_str().and_then(|s| s.parse().ok()))
        .or_else(|| params["min_fee_ref_script_coins_per_byte"].as_f64())
        .or_else(|| params["min_fee_ref_script_coins_per_byte"].as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(15.0);
    let cost_v2: Vec<i64> = if let Some(arr) = params["cost_models_raw"]["PlutusV2"].as_array() {
        arr.iter()
            .map(|v| v.as_i64().ok_or_else(|| anyhow!("cost_models_raw entry not i64")))
            .collect::<Result<Vec<_>>>()?
    } else if let Some(arr) = params["cost_models"]["PlutusV2"].as_array() {
        arr.iter()
            .map(|v| v.as_i64().ok_or_else(|| anyhow!("cost_models entry not i64")))
            .collect::<Result<Vec<_>>>()?
    } else {
        bail!(
            "could not find PlutusV2 cost model under `cost_models_raw.PlutusV2` or `cost_models.PlutusV2`. \
             response keys: {:?}",
            params.as_object().map(|m| m.keys().collect::<Vec<_>>())
        );
    };
    eprintln!(
        "protocol: a={} b={} price_mem={}/{} price_step={}/{} ref_script_per_byte={} v2_cost_entries={}",
        min_fee_a, min_fee_b, price_mem.0, price_mem.1, price_step.0, price_step.1,
        ref_script_cost_per_byte, cost_v2.len()
    );

    let current_slot = kupo.tip_slot().await.context("tip slot")?;
    let ttl = current_slot + 7200;
    eprintln!("slot: current={} ttl={}", current_slot, ttl);
    eprintln!();

    // 7. Coin selection.
    //    Inputs  = cancel script inputs (sorted_cancels) + wallet UTxOs (fee + new order ADA)
    //    Outputs = new order outputs (plan.new_orders) + 1 change output
    //
    //    Balance equation:
    //      total_wallet_selected + total_cancel_lovelace
    //        == total_new_order_lovelace + fee + change
    //    So change = (wallet + cancel) - new_orders - fee. We must include the
    //    cancel-side ADA in the change calculation, otherwise every bundle with
    //    ≥1 cancel is rejected by the ledger for unbalanced lovelace.
    let total_new_order_lovelace: u64 = plan.new_orders.iter()
        .map(|p| p.assets.iter().find(|a| a.unit == "lovelace").map(|a| a.quantity).unwrap_or(0))
        .sum();
    let total_cancel_lovelace: u64 = sorted_cancels.iter()
        .map(|s| s.utxo.amount.iter()
            .find(|a| a.unit == "lovelace")
            .and_then(|a| a.quantity.parse::<u64>().ok())
            .unwrap_or(0))
        .sum();
    eprintln!(
        "total new-order lovelace: {} ({:.6} ADA); total cancel-input lovelace: {} ({:.6} ADA)",
        total_new_order_lovelace, total_new_order_lovelace as f64 / 1_000_000.0,
        total_cancel_lovelace, total_cancel_lovelace as f64 / 1_000_000.0
    );

    // Token balance from cancel inputs minus tokens going into new-order outputs.
    // Whatever's left over must be attached to the change output (otherwise the
    // ledger rejects: tokens consumed but not produced).
    let mut change_tokens: std::collections::BTreeMap<String, u64> =
        std::collections::BTreeMap::new();
    for spend in &sorted_cancels {
        for unit in &spend.utxo.amount {
            if unit.unit == "lovelace" {
                continue;
            }
            let qty: u64 = unit.quantity.parse().unwrap_or(0);
            *change_tokens.entry(unit.unit.clone()).or_insert(0) += qty;
        }
    }
    // Subtract tokens carried into new orders (e.g., token-side of swap-in for token→ADA sells).
    for p in &plan.new_orders {
        for asset in &p.assets {
            if asset.unit == "lovelace" {
                continue;
            }
            let entry = change_tokens.entry(asset.unit.clone()).or_insert(0);
            *entry = entry.saturating_sub(asset.quantity);
        }
    }
    // Drop zero-quantity entries — empty asset attachments would be malformed.
    change_tokens.retain(|_, q| *q > 0);
    if !change_tokens.is_empty() {
        eprintln!("change tokens (from cancel inputs, net of new-order tokens):");
        for (unit, qty) in &change_tokens {
            eprintln!("  {} : {}", unit, qty);
        }
    }

    let wallet_utxos = kupo.get(&cfg.sender_bech32, true).await
        .context("fetching wallet UTxOs from Kupo")?;
    eprintln!("kupo returned {} UTxO(s) at sender", wallet_utxos.len());
    if wallet_utxos.is_empty() {
        bail!(
            "Kupo returned 0 UTxOs for {}. Check: (a) the wallet is actually funded, and (b) Kupo is indexing this address.",
            cfg.sender_bech32
        );
    }

    // Exclude collateral UTxO from fee coin selection. (Cancel UTxOs live at
    // the script address, NOT at sender_bech32, so they cannot appear in
    // wallet_utxos here — no need to filter for them.)
    let collateral_ref_key: Option<(String, u64)> = collateral.as_ref()
        .map(|(u, _)| (u.tx_hash.clone(), u.output_index as u64));

    let mut ada_candidates: Vec<(String, u64, u64)> = vec![]; // (tx_hash, output_index, lovelace)
    for u in &wallet_utxos {
        if u.amount.len() != 1 || u.amount[0].unit != "lovelace" {
            continue; // skip multi-asset UTxOs for simplicity
        }
        // Exclude collateral.
        if let Some((ref c_hash, c_idx)) = collateral_ref_key {
            if u.tx_hash == *c_hash && u.output_index as u64 == c_idx {
                continue;
            }
        }
        let qty: u64 = u.amount[0].quantity.parse().unwrap_or(0);
        ada_candidates.push((u.tx_hash.clone(), u.output_index as u64, qty));
    }
    ada_candidates.sort_by(|a, b| b.2.cmp(&a.2));

    let min_change: u64 = 1_500_000;
    let fee_estimate_initial: u64 = 500_000; // generous initial estimate for coin selection
    // Target wallet ADA = (new orders + fee + min_change) − cancel-side ADA.
    // saturating_sub: if cancel-side ADA alone already covers new orders + fee + change,
    // we still need at least one wallet input for fee payment continuity (skip 0).
    let target = (total_new_order_lovelace + fee_estimate_initial + min_change)
        .saturating_sub(total_cancel_lovelace);

    let mut wallet_selected: Vec<(String, u64, u64)> = vec![];
    let mut total_wallet_selected: u64 = 0;
    for u in &ada_candidates {
        if total_wallet_selected >= target && total_wallet_selected > 0 {
            break;
        }
        wallet_selected.push(u.clone());
        total_wallet_selected += u.2;
    }
    let total_inputs_lovelace_initial = total_wallet_selected + total_cancel_lovelace;
    if total_inputs_lovelace_initial < total_new_order_lovelace + fee_estimate_initial + min_change {
        bail!(
            "insufficient ADA: wallet has {} lovelace (pure-ADA UTxOs) + cancel inputs {} = {}, \
             need at least {} for new orders + fee + min_change",
            total_wallet_selected, total_cancel_lovelace,
            total_inputs_lovelace_initial,
            total_new_order_lovelace + fee_estimate_initial + min_change
        );
    }
    eprintln!(
        "selected {} wallet input(s) totalling {} lovelace ({:.6} ADA); total inputs = {} lovelace",
        wallet_selected.len(), total_wallet_selected, total_wallet_selected as f64 / 1_000_000.0,
        total_inputs_lovelace_initial
    );
    eprintln!();

    // 8. Parse addresses for pallas.
    let sender_pallas = PallasAddress::from_bech32(&cfg.sender_bech32)
        .map_err(|e| anyhow!("parsing sender address: {}", e))?;

    // For each new order output, parse the script address.
    let mut new_order_addrs: Vec<PallasAddress> = Vec::new();
    for (i, p) in plan.new_orders.iter().enumerate() {
        let addr = PallasAddress::from_bech32(&p.address)
            .map_err(|e| anyhow!("parsing new_order[{}] address '{}': {}", i, p.address, e))?;
        new_order_addrs.push(addr);
    }

    // 9. Eval pass with placeholder ex_units.
    //    We only do an eval pass if there are script inputs (cancels).
    let placeholder_ex = ExUnits { mem: 14_000_000, steps: 10_000_000_000 };
    let eval_fee: u64 = 1_000_000;
    // Eval-pass change includes cancel-side lovelace too (same balance equation
    // as the real fee loop): change = (wallet + cancel) − new_orders − fee.
    let eval_total_inputs = total_wallet_selected + total_cancel_lovelace;
    let eval_change = eval_total_inputs
        .saturating_sub(total_new_order_lovelace)
        .saturating_sub(eval_fee);

    // Cancel inputs in CANONICAL (sorted) order so positions match spend:N
    // redeemer indices from Blockfrost evaluate.
    let cancel_inputs_for_eval: Vec<Input> = sorted_cancels.iter()
        .map(|s| -> Result<Input> {
            Ok(Input::new(parse_hash32(&s.utxo.tx_hash)?, s.utxo.output_index as u64))
        })
        .collect::<Result<Vec<_>>>()?;

    let wallet_inputs_for_eval: Vec<Input> = wallet_selected.iter()
        .map(|(h, idx, _)| -> Result<Input> {
            Ok(Input::new(parse_hash32(h)?, *idx))
        })
        .collect::<Result<Vec<_>>>()?;

    let collateral_input: Option<Input> = collateral.as_ref()
        .map(|(u, _)| -> Result<Input> {
            Ok(Input::new(parse_hash32(&u.tx_hash)?, u.output_index as u64))
        })
        .transpose()?;

    // Build the per-cancel placeholder ex_units list (one per cancel input, all placeholder).
    let placeholder_ex_units: Vec<ExUnits> = sorted_cancels.iter()
        .map(|_| placeholder_ex.clone())
        .collect();

    // Eval build.
    let (real_ex_units, total_script_fee, ref_script_fee) = if !sorted_cancels.is_empty() {
        let tx_for_eval = build_bulk_tx(
            &cancel_inputs_for_eval,
            &wallet_inputs_for_eval,
            collateral_input.clone(),
            &plan,
            &change_tokens,
            &new_order_addrs,
            sender_pallas.clone(),
            eval_change,
            &script_cbor,
            validator_reference.clone(),
            &redeemers_cbor,
            &placeholder_ex_units,
            Hash::<28>::from(payment_pkh),
            cost_v2.clone(),
            eval_fee,
            ttl,
        )?;
        let unsigned_built = tx_for_eval.build_conway_raw()
            .map_err(|e| anyhow!("initial build for eval: {:?}", e))?;
        eprintln!("evaluating {} script redeemer(s) via Blockfrost...", sorted_cancels.len());
        let real_exs = blockfrost_evaluate_all(
            &client, &cfg.bf_project_id, &unsigned_built.tx_bytes.0, sorted_cancels.len()
        ).await.context("Blockfrost /utils/txs/evaluate")?;

        eprintln!("✓ script costs per redeemer:");
        let mut total_script_fee_acc: u64 = 0;
        for (i, ex) in real_exs.iter().enumerate() {
            let sf = ceil_div(ex.mem as u128 * price_mem.0 as u128, price_mem.1 as u128)
                + ceil_div(ex.steps as u128 * price_step.0 as u128, price_step.1 as u128);
            total_script_fee_acc += sf as u64;
            eprintln!("  spend[{}]: mem={} steps={} fee={}", i, ex.mem, ex.steps, sf);
        }

        // Ref-script fee: paid ONCE regardless of how many cancels reference it.
        // This is the key cost win: N cancels cost the same ref-script fee as 1.
        let rsf: u64 = if validator_reference.is_some() {
            (ref_script_size as f64 * ref_script_cost_per_byte).ceil() as u64
        } else {
            0
        };
        eprintln!("total script fee: {} lovelace; ref_script fee: {} lovelace (paid ONCE for all {} cancel(s))",
            total_script_fee_acc, rsf, sorted_cancels.len());

        (real_exs, total_script_fee_acc, rsf)
    } else {
        eprintln!("(no script inputs; skipping eval)");
        (vec![], 0, 0)
    };

    // 10. Fixpoint fee loop.
    const VKEY_WITNESS_OVERHEAD: u64 = 220;
    const FEE_SAFETY: u64 = 500;
    const MAX_ITERS: usize = 6;

    let mut fee_estimate: u64 = 200_000;
    let mut built = None;
    let mut converged = false;

    for iter in 0..MAX_ITERS {
        // Balance: change = (wallet + cancel) − new_orders − fee.
        // Cancel-input lovelace is consumed; not adding it to change would
        // leave the tx short by sum(cancel_lovelace) and the ledger rejects.
        let total_inputs_lovelace = total_wallet_selected + total_cancel_lovelace;
        let needed = total_new_order_lovelace + fee_estimate + min_change;
        if total_inputs_lovelace < needed {
            bail!(
                "total inputs ({} lovelace = wallet {} + cancels {}) cannot cover new_orders + fee + min_change = {}",
                total_inputs_lovelace, total_wallet_selected, total_cancel_lovelace, needed
            );
        }
        let change = total_inputs_lovelace - total_new_order_lovelace - fee_estimate;

        let tx = build_bulk_tx(
            &cancel_inputs_for_eval,
            &wallet_inputs_for_eval,
            collateral_input.clone(),
            &plan,
            &change_tokens,
            &new_order_addrs,
            sender_pallas.clone(),
            change,
            &script_cbor,
            validator_reference.clone(),
            &redeemers_cbor,
            &real_ex_units,
            Hash::<28>::from(payment_pkh),
            cost_v2.clone(),
            fee_estimate,
            ttl,
        )?;
        let raw = tx.build_conway_raw()
            .map_err(|e| anyhow!("build pass {}: {:?}", iter, e))?;
        let signed_size = raw.tx_bytes.0.len() as u64 + VKEY_WITNESS_OVERHEAD;
        let tx_fee = min_fee_b + min_fee_a * signed_size;
        let required = tx_fee + total_script_fee + ref_script_fee + FEE_SAFETY;
        eprintln!(
            "fee iter {}: estimate={} tx_fee={} script_fee={} ref_script={} required={} size~{}",
            iter, fee_estimate, tx_fee, total_script_fee, ref_script_fee, required, signed_size
        );
        if fee_estimate >= required {
            built = Some(raw);
            converged = true;
            break;
        }
        fee_estimate = required;
    }
    if !converged {
        bail!("fee refinement did not converge in {} iterations", MAX_ITERS);
    }
    let built = built.unwrap();

    // Collateral coverage check.
    if let Some(ref c) = collateral {
        let required_collateral = (fee_estimate * collateral_percent + 99) / 100;
        if c.1 < required_collateral {
            bail!(
                "collateral UTxO ({} lovelace) below required {} ({}% of fee {})",
                c.1, required_collateral, collateral_percent, fee_estimate
            );
        }
    }

    // 11. Sign.
    let signed = built.sign(&signing_key)
        .map_err(|e| anyhow!("sign: {:?}", e))?;
    let tx_cbor = hex::encode(&signed.tx_bytes.0);
    let tx_hash_out = hex::encode(signed.tx_hash.0);

    eprintln!();
    eprintln!("=== Signed bulk tx ready ===");
    eprintln!("tx hash:    {}", tx_hash_out);
    eprintln!("tx size:    {} bytes", signed.tx_bytes.0.len());
    eprintln!("fee:        {} lovelace ({:.6} ADA)", fee_estimate, fee_estimate as f64 / 1_000_000.0);
    eprintln!("cancels:    {} script input(s)", sorted_cancels.len());
    eprintln!("new orders: {} script output(s)", plan.new_orders.len());
    if sorted_cancels.len() > 1 && ref_script_fee > 0 {
        eprintln!(
            "ref-script fee paid ONCE: {} lovelace (vs {} if separate txs)",
            ref_script_fee, ref_script_fee * sorted_cancels.len() as u64
        );
    }
    eprintln!("tx cbor:    {}", tx_cbor);
    eprintln!("inspect:    {}/{}", CARDANOSCAN_TX, tx_hash_out);
    eprintln!();

    if !submit {
        eprintln!("DRY-RUN complete. Not broadcasting.");
        eprintln!("Re-run with --submit to broadcast.");
        return Ok(());
    }

    // 12. Submit.
    eprintln!("=== SUBMITTING ===");
    let resp = client.post(format!("{}/tx/submit", BLOCKFROST_BASE))
        .header("project_id", &cfg.bf_project_id)
        .header("Content-Type", "application/cbor")
        .body(signed.tx_bytes.0.clone())
        .send().await
        .context("HTTP POST /tx/submit")?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("submit failed [{}]: {}", status, body);
    }
    let h = body.trim_matches('"');
    eprintln!("✓ submitted!");
    eprintln!("tx hash: {}", h);
    eprintln!("{}/{}", CARDANOSCAN_TX, h);
    Ok(())
}

// ---------- tx builder ----------

/// Build a StagingTransaction for the bulk bundle:
///   - N cancel script inputs (each with their own spend redeemer + ex_units)
///   - M wallet inputs (fee source + new-order ADA)
///   - Collateral input (optional; required when there are script inputs)
///   - K new order outputs (inline datum)
///   - 1 change output
///   - 1 reference_input (the shared validator reference, if any)
///   - required_signers: payment PKH
///   - language_views: PlutusV2
///
/// All cancel inputs share the SAME reference script UTxO (ONE reference_input).
/// The Conway-era ref-script fee is charged once, not per-cancel — that's the win.
#[allow(clippy::too_many_arguments)]
fn build_bulk_tx(
    cancel_inputs: &[Input],
    wallet_inputs: &[Input],
    collateral_input: Option<Input>,
    plan: &dexter_kupo_rs::BulkOrderPlan,
    change_tokens: &std::collections::BTreeMap<String, u64>,
    new_order_addrs: &[PallasAddress],
    sender_addr: PallasAddress,
    change_lovelace: u64,
    script_cbor: &[u8],
    validator_reference: Option<Input>,
    redeemers_cbor: &[Vec<u8>],
    ex_units_per_cancel: &[ExUnits],
    payment_pkh: Hash<28>,
    plutus_v2_cost_model: Vec<i64>,
    fee: u64,
    ttl: u64,
) -> Result<StagingTransaction> {
    let mut tx = StagingTransaction::new();

    // Cancel script inputs.
    for input in cancel_inputs {
        tx = tx.input(input.clone());
    }

    // Wallet fee inputs.
    for input in wallet_inputs {
        tx = tx.input(input.clone());
    }

    // Collateral.
    if let Some(ci) = collateral_input {
        tx = tx.collateral_input(ci);
    }

    // New order outputs (inline datum).
    for (i, p) in plan.new_orders.iter().enumerate() {
        let order_lovelace = p.assets.iter()
            .find(|a| a.unit == "lovelace")
            .map(|a| a.quantity)
            .unwrap_or(0);
        let datum_hex = p.datum.as_deref()
            .ok_or_else(|| anyhow!("new_order[{}]: missing datum", i))?;
        let datum_bytes = hex::decode(datum_hex)
            .with_context(|| format!("decoding datum cbor for new_order[{}]", i))?;

        let mut out = Output::new(new_order_addrs[i].clone(), order_lovelace)
            .set_inline_datum(datum_bytes);

        // Carry non-lovelace assets (e.g., swap-in native token).
        for asset in &p.assets {
            if asset.unit == "lovelace" {
                continue;
            }
            if asset.unit.len() < 56 {
                bail!("new_order[{}]: malformed asset unit: {}", i, asset.unit);
            }
            let (policy_hex, name_hex) = asset.unit.split_at(56);
            let policy: [u8; 28] = hex::decode(policy_hex)?
                .try_into()
                .map_err(|_| anyhow!("policy id not 28 bytes"))?;
            let name: Vec<u8> = hex::decode(name_hex)?;
            out = out.add_asset(Hash::<28>::from(policy), name, asset.quantity)
                .map_err(|e| anyhow!("add_asset to new_order[{}]: {:?}", i, e))?;
        }
        tx = tx.output(out);
    }

    // Change output — carries leftover ADA + any tokens from cancel inputs
    // that weren't consumed by new-order outputs. Missing this attachment
    // would leave tokens consumed-but-not-produced and the ledger rejects.
    let mut change_out = Output::new(sender_addr, change_lovelace);
    for (unit, qty) in change_tokens {
        if *qty == 0 {
            continue;
        }
        if unit.len() < 56 {
            bail!("malformed change-token unit: {}", unit);
        }
        let (policy_hex, name_hex) = unit.split_at(56);
        let policy: [u8; 28] = hex::decode(policy_hex)?
            .try_into()
            .map_err(|_| anyhow!("policy id on change token not 28 bytes"))?;
        let name: Vec<u8> = hex::decode(name_hex)?;
        change_out = change_out
            .add_asset(Hash::<28>::from(policy), name, *qty)
            .map_err(|e| anyhow!("add_asset on change output: {:?}", e))?;
    }
    tx = tx.output(change_out);

    // Reference input (ONE for all cancels — the cost win).
    if let Some(ref_input) = validator_reference {
        tx = tx.reference_input(ref_input);
    } else if !cancel_inputs.is_empty() {
        // Fall back to inline script if no reference UTxO.
        tx = tx.script(ScriptKind::PlutusV2, script_cbor.to_vec());
    }

    // Per-cancel spend redeemers (indexed by input position in the tx).
    for (i, (input, redeemer_cbor)) in cancel_inputs.iter().zip(redeemers_cbor.iter()).enumerate() {
        let ex = ex_units_per_cancel.get(i)
            .cloned()
            .unwrap_or(ExUnits { mem: 14_000_000, steps: 10_000_000_000 });
        tx = tx.add_spend_redeemer(input.clone(), redeemer_cbor.clone(), Some(ex));
    }

    // Required signer (payment PKH) — cancel validator checks for it.
    if !cancel_inputs.is_empty() {
        let mut language_map = BTreeMap::new();
        language_map.insert(1u8, plutus_v2_cost_model);
        let language_views = LanguageViews(language_map);
        tx = tx
            .disclosed_signer(payment_pkh)
            .language_views(language_views);
    }

    tx = tx
        .fee(fee)
        .invalid_from_slot(ttl)
        .network_id(1);

    Ok(tx)
}

// ---------- Blockfrost ----------

async fn blockfrost_get(client: &reqwest::Client, project_id: &str, path: &str)
    -> Result<serde_json::Value>
{
    let url = format!("{}{}", BLOCKFROST_BASE, path);
    let resp = client.get(&url).header("project_id", project_id).send().await
        .with_context(|| format!("GET {}", url))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_else(|_| "<unreadable>".into());
    if !status.is_success() {
        bail!("Blockfrost GET {} {}: {}", path, status, body);
    }
    serde_json::from_str(&body).with_context(|| format!("parsing JSON from {}", path))
}

/// POST raw tx CBOR to /utils/txs/evaluate; return ExUnits for ALL spend redeemers
/// indexed 0..n_cancels-1. Returns a Vec<ExUnits> in index order.
///
/// Blockfrost response shapes (both Ogmios 5 and Ogmios 6):
///   Old: { "result": { "EvaluationResult": { "spend:0": {"memory":N,"steps":M}, ... } } }
///   New: { "result": [ { "validator": {"purpose":"spend","index":0}, "budget": {"memory":N,"cpu":M} }, ... ] }
async fn blockfrost_evaluate_all(
    client: &reqwest::Client,
    project_id: &str,
    tx_bytes: &[u8],
    n_cancels: usize,
) -> Result<Vec<ExUnits>> {
    let url = format!("{}/utils/txs/evaluate", BLOCKFROST_BASE);
    let hex_body = hex::encode(tx_bytes);
    let resp = client.post(&url)
        .header("project_id", project_id)
        .header("Content-Type", "application/cbor")
        .body(hex_body)
        .send().await
        .context("POST /utils/txs/evaluate")?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("evaluate failed [{}]: {}", status, body);
    }
    let v: serde_json::Value = serde_json::from_str(&body)
        .context("parsing evaluate response JSON")?;

    let mut results: Vec<Option<ExUnits>> = vec![None; n_cancels];

    // Old Ogmios 5 shape: result.EvaluationResult.{"spend:0": {memory, steps}, ...}
    if let Some(map) = v["result"]["EvaluationResult"].as_object() {
        for (k, val) in map {
            if let Some(idx_str) = k.strip_prefix("spend:") {
                let idx: usize = idx_str.parse()
                    .with_context(|| format!("parsing spend redeemer index from key `{}`", k))?;
                let mem = val["memory"].as_u64()
                    .ok_or_else(|| anyhow!("spend:{}: memory missing", idx))?;
                let steps = val["steps"].as_u64()
                    .ok_or_else(|| anyhow!("spend:{}: steps missing", idx))?;
                if idx < results.len() {
                    results[idx] = Some(ExUnits { mem, steps });
                }
            }
        }
    }
    // New Ogmios 6 shape: result is an array of { validator: {purpose,index}, budget: {memory,cpu} }
    else if let Some(arr) = v["result"].as_array() {
        for entry in arr {
            if entry["validator"]["purpose"].as_str() == Some("spend") {
                let idx = entry["validator"]["index"].as_u64()
                    .ok_or_else(|| anyhow!("validator.index missing in evaluate response"))? as usize;
                let mem = entry["budget"]["memory"].as_u64()
                    .ok_or_else(|| anyhow!("budget.memory missing"))?;
                let steps = entry["budget"]["cpu"].as_u64()
                    .or_else(|| entry["budget"]["steps"].as_u64())
                    .ok_or_else(|| anyhow!("budget.cpu/steps missing"))?;
                if idx < results.len() {
                    results[idx] = Some(ExUnits { mem, steps });
                }
            }
        }
    } else {
        bail!("could not parse evaluate response: {}", body);
    }

    // Verify all N redeemers were returned. Report the ACTUAL number of
    // resolved entries in the error (was previously echoing the expected
    // count for both args, which obscured the size of the gap).
    let actual_returned = results.iter().filter(|r| r.is_some()).count();
    let resolved: Result<Vec<ExUnits>> = results.into_iter().enumerate()
        .map(|(i, opt)| {
            opt.ok_or_else(|| anyhow!(
                "Blockfrost evaluate response missing spend:{} \
                 (only returned {} redeemers out of {} expected). Full response: {}",
                i, actual_returned, n_cancels, body
            ))
        })
        .collect();
    resolved
}

// ---------- helpers ----------

/// Collect all values for a repeatable flag from argv.
/// E.g. `--cancel a --cancel b` returns `["a", "b"]`.
fn collect_flag_values(args: &[String], flag: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == flag {
            if let Some(v) = args.get(i + 1) {
                out.push(v.clone());
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Parse `<tx_hash>#<output_index>` into an OrderRef.
fn parse_utxo_ref(s: &str) -> Result<OrderRef> {
    let (h, i) = s.split_once('#')
        .ok_or_else(|| anyhow!("expected `<tx_hash>#<output_index>`, got `{}`", s))?;
    let output_index: u64 = i.parse()
        .map_err(|_| anyhow!("output_index `{}` is not a u64", i))?;
    if h.len() != 64 {
        bail!("tx_hash `{}` is not 64 hex characters (32 bytes)", h);
    }
    hex::decode(h).context("tx_hash is not valid hex")?;
    Ok(OrderRef { tx_hash: h.to_string(), output_index })
}

fn parse_hash32(hex_str: &str) -> Result<Hash<32>> {
    let raw = hex::decode(hex_str).context("decoding hash hex")?;
    let arr: [u8; 32] = raw.try_into()
        .map_err(|_| anyhow!("hash is not 32 bytes"))?;
    Ok(Hash::<32>::from(arr))
}

fn parse_price(v: &serde_json::Value) -> Result<(u64, u64)> {
    if let Some(s) = v.as_str() {
        let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
        let denom: u64 = 10u64.pow(frac.len() as u32);
        let num: u64 = format!("{}{}", whole, frac).parse()?;
        return Ok((num, denom));
    }
    if let Some(n) = v.as_f64() {
        let denom = 1_000_000_000u64;
        let num = (n * denom as f64).round() as u64;
        return Ok((num, denom));
    }
    if let (Some(num), Some(denom)) = (v["numerator"].as_u64(), v["denominator"].as_u64()) {
        return Ok((num, denom));
    }
    bail!("could not parse price value: {:?}", v)
}

fn ceil_div(a: u128, b: u128) -> u128 {
    (a + b - 1) / b
}

fn token_from_unit(unit: &str) -> Result<Token> {
    if unit == "lovelace" {
        return Ok(Token::Lovelace);
    }
    if unit.len() < 56 {
        bail!("token unit '{}' is too short (need at least 56 hex chars for policy ID)", unit);
    }
    let (policy, name) = unit.split_at(56);
    Ok(Token::Asset(Asset::new(policy, name, 0)))
}

fn token_unit(t: &Token) -> String {
    match t {
        Token::Lovelace => "lovelace".to_string(),
        Token::Asset(a) => format!("{}{}", a.policy_id, a.name_hex),
    }
}

// ---------- config ----------

struct Config {
    kupo_url: String,
    bf_project_id: String,
    sender_bech32: String,
    sender_mnemonic: String,
    sender_mnemonic_passphrase: String,
    /// Pool LP token unit — required if needs_swap_params.
    pool_lp_token_unit: Option<String>,
    /// Swap-in unit string — required if needs_swap_params.
    swap_in_unit: Option<String>,
    /// Swap-in amount — required if needs_swap_params.
    swap_in_amount: Option<u64>,
    /// Limit premium percent — defaults to 10.0.
    limit_premium_percent: f64,
    /// Optional pinned collateral UTxO as (tx_hash, output_index).
    collateral_utxo: Option<(String, u64)>,
}

impl Config {
    fn from_env(needs_swap_params: bool) -> Result<Self> {
        let mut required: Vec<&str> = vec![
            "KUPO_URL", "BLOCKFROST_PROJECT_ID", "SENDER_BECH32", "SENDER_MNEMONIC",
        ];
        if needs_swap_params {
            required.extend_from_slice(&["POOL_LP_TOKEN_UNIT", "SWAP_IN_UNIT", "SWAP_IN_AMOUNT"]);
        }

        let missing: Vec<&str> = required.iter().copied()
            .filter(|k| env::var(k).is_err())
            .collect();
        if !missing.is_empty() {
            bail!("missing required env vars: {:?}", missing);
        }

        let kupo_url = env::var("KUPO_URL").unwrap();
        let bf_project_id = env::var("BLOCKFROST_PROJECT_ID").unwrap();
        let sender_bech32 = env::var("SENDER_BECH32").unwrap();

        if !sender_bech32.starts_with("addr1") {
            bail!(
                "SENDER_BECH32 must be a mainnet base address (addr1...) — \
                 testnet/preview addresses are not supported"
            );
        }
        if !bf_project_id.starts_with("mainnet") {
            bail!("BLOCKFROST_PROJECT_ID must start with `mainnet`");
        }

        let limit_premium_percent = env::var("LIMIT_PREMIUM_PERCENT")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(10.0);

        let collateral_utxo = env::var("COLLATERAL_UTXO").ok()
            .map(|s| {
                let (h, i) = s.split_once('#')
                    .ok_or_else(|| anyhow!("COLLATERAL_UTXO must be `<tx_hash>#<index>`, got `{}`", s))?;
                let idx: u64 = i.parse()
                    .map_err(|_| anyhow!("COLLATERAL_UTXO index `{}` is not a u64", i))?;
                if h.len() != 64 || hex::decode(h).is_err() {
                    bail!("COLLATERAL_UTXO tx_hash `{}` is not 32 hex bytes", h);
                }
                Ok::<_, anyhow::Error>((h.to_string(), idx))
            })
            .transpose()?;

        let (pool_lp_token_unit, swap_in_unit, swap_in_amount) = if needs_swap_params {
            (
                Some(env::var("POOL_LP_TOKEN_UNIT").unwrap()),
                Some(env::var("SWAP_IN_UNIT").unwrap()),
                Some(env::var("SWAP_IN_AMOUNT").unwrap()
                    .parse::<u64>()
                    .context("SWAP_IN_AMOUNT must be a u64")?),
            )
        } else {
            (
                env::var("POOL_LP_TOKEN_UNIT").ok(),
                env::var("SWAP_IN_UNIT").ok(),
                env::var("SWAP_IN_AMOUNT").ok()
                    .and_then(|s| s.parse::<u64>().ok()),
            )
        };

        Ok(Self {
            kupo_url, bf_project_id, sender_bech32,
            sender_mnemonic: env::var("SENDER_MNEMONIC").unwrap(),
            sender_mnemonic_passphrase: env::var("SENDER_MNEMONIC_PASSPHRASE").unwrap_or_default(),
            pool_lp_token_unit,
            swap_in_unit,
            swap_in_amount,
            limit_premium_percent,
            collateral_utxo,
        })
    }
}

// ---------- key derivation (same as live_cancel.rs / live_update.rs) ----------

fn derive_account_keys(mnemonic_phrase: &str, passphrase: &str)
    -> Result<(XPrv, XPrv, String)>
{
    let mnemonic = Mnemonic::parse_in_normalized(Language::English, mnemonic_phrase)
        .map_err(|_| anyhow!("invalid mnemonic"))?;
    let entropy = mnemonic.to_entropy();
    let mut xprv_bytes = [0u8; 96];
    pbkdf2::pbkdf2_hmac::<sha2::Sha512>(passphrase.as_bytes(), &entropy, 4096, &mut xprv_bytes);
    xprv_bytes[0]  &= 0b1111_1000;
    xprv_bytes[31] &= 0b0001_1111;
    xprv_bytes[31] |= 0b0100_0000;
    let root = XPrv::from_bytes_verified(xprv_bytes)
        .map_err(|_| anyhow!("invalid root xprv"))?;
    let account = root
        .derive(DerivationScheme::V2, 1852 | HARDENED)
        .derive(DerivationScheme::V2, 1815 | HARDENED)
        .derive(DerivationScheme::V2, 0 | HARDENED);
    let payment = account.derive(DerivationScheme::V2, 0).derive(DerivationScheme::V2, 0);
    let stake = account.derive(DerivationScheme::V2, 2).derive(DerivationScheme::V2, 0);
    let payment_pub: [u8; 32] = payment.public().public_key();
    let stake_pub: [u8; 32] = stake.public().public_key();
    let pay_h = pallas_crypto::hash::Hasher::<224>::hash(&payment_pub);
    let stake_h = pallas_crypto::hash::Hasher::<224>::hash(&stake_pub);
    let mut payload = Vec::with_capacity(57);
    payload.push(0x01u8);
    payload.extend_from_slice(pay_h.as_ref());
    payload.extend_from_slice(stake_h.as_ref());
    let hrp = bech32::Hrp::parse("addr")?;
    let derived = bech32::encode::<bech32::Bech32>(hrp, &payload)?;
    Ok((payment, stake, derived))
}
