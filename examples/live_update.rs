//! Live Minswap V2 order UPDATE example — atomically spends an existing order
//! (cancel redeemer) + creates a new order with different swap parameters,
//! all in a single transaction.
//!
//! USAGE:
//!   cargo run --example live_update -- --tx <hash>             # dry-run
//!   cargo run --example live_update -- --tx <hash> --submit    # broadcast
//!
//! ARGS:
//!   --tx <hash>          required: hash of the tx that placed the old order
//!   --out-index <n>      optional: index of the order output in that tx (default 0)
//!   --submit             optional: actually broadcast (default: dry-run)
//!
//! ENV VARS (superset of live_cancel + live_swap):
//!   KUPO_URL                   local Kupo API URL (http://localhost:1442)
//!   BLOCKFROST_PROJECT_ID      mainnet Blockfrost project ID (mainnet...)
//!   SENDER_BECH32              sender mainnet base address (addr1...)
//!   SENDER_MNEMONIC            BIP-39 mnemonic (12, 15, 18, 21, or 24 words)
//!   POOL_LP_TOKEN_UNIT         V2 pool LP token unit: <lp_policy_56_hex><lp_name_hex>
//!   SWAP_IN_UNIT               "lovelace" or <policy_hex><name_hex>
//!   SWAP_IN_AMOUNT             raw u64 amount to swap in (for the NEW order)
//!
//! OPTIONAL:
//!   SENDER_MNEMONIC_PASSPHRASE  BIP-39 passphrase (default: empty)
//!   LIMIT_PREMIUM_PERCENT       new order min_receive = estimate * (1 + X/100); default 10.0
//!   COLLATERAL_UTXO             pin a wallet UTxO `<tx_hash>#<idx>` as collateral
//!
//! HOW IT WORKS:
//!   A Minswap V2 "update" is NOT a special contract redeemer — it is a
//!   transaction-level pattern: spend the old order (cancel redeemer) AND
//!   create a brand-new order output in the same tx, referencing the published
//!   script UTxO (CIP-33) to avoid carrying the 2659-byte validator inline.
//!
//!   Result: one atomic tx, expected fee ~0.25 ADA (vs ~0.55 ADA for a
//!   separate cancel + re-place).
//!
//! BALANCE MODEL:
//!   - Inputs:  old order UTxO (script) + 1 wallet UTxO (fee source)
//!   - Outputs: new order at script address (inline datum) + wallet change
//!   The fee is taken from the wallet input, keeping the new order's ADA
//!   amount identical to what the library specifies.
//!
//! # Safety
//!   Default mode is DRY-RUN.  Pass --submit to broadcast.  Real ADA spent.

use anyhow::{anyhow, bail, Context, Result};
use bip39::{Language, Mnemonic};
use dexter_kupo_rs::{
    dex::{minswap_v2::MinswapV2, BaseDex, DexSwap},
    models::{token_identifier, Asset, Token},
    KupoApi, UpdateSwapRequest,
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
    let old_tx_hash = parse_flag(&args, "--tx")
        .context("--tx <hash> is required")?;
    let out_index: u64 = parse_flag(&args, "--out-index")
        .unwrap_or_else(|_| "0".to_string())
        .parse()
        .context("--out-index must be a u64")?;

    let cfg = Config::from_env().context("loading env vars")?;

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

    // Payment key hash (Blake2b-224 of pubkey) — required signer so the cancel
    // script's signer check passes.
    let payment_pub: [u8; 32] = payment_xprv.public().public_key();
    let payment_pkh: [u8; 28] = {
        let h = pallas_crypto::hash::Hasher::<224>::hash(&payment_pub);
        let mut out = [0u8; 28];
        out.copy_from_slice(h.as_ref());
        out
    };

    let client = reqwest::Client::new();

    eprintln!("== Live Update (Minswap V2) ==");
    eprintln!("mode:   {}", if submit { "SUBMIT" } else { "DRY-RUN" });
    eprintln!("order:  {}@{}", out_index, old_tx_hash);
    eprintln!("sender: {}", cfg.sender_bech32);
    eprintln!("pool:   {}", cfg.pool_lp_token_unit);
    eprintln!("swap in (new order): {} of {}", cfg.swap_in_amount, cfg.swap_in_unit);
    eprintln!();

    let kupo = KupoApi::new(&cfg.kupo_url);
    let dex = MinswapV2::new(KupoApi::new(&cfg.kupo_url));

    // 1. Look up the old order UTxO.
    let order_ref = format!("{}@{}", out_index, old_tx_hash);
    let order_utxos = kupo.get(&order_ref, true).await
        .context("fetching order UTxO from Kupo")?;
    if order_utxos.is_empty() {
        bail!(
            "order UTxO {} not found at Kupo. Either (a) already spent (filled/cancelled), \
             or (b) Kupo is not indexing the order script address. \
             Add `--match <order_script_addr>` or `--match \"*\"`.",
            order_ref
        );
    }
    let old_order_utxo = order_utxos[0].clone();
    let old_order_lovelace: u64 = old_order_utxo
        .amount
        .iter()
        .find(|a| a.unit == "lovelace")
        .ok_or_else(|| anyhow!("order UTxO has no lovelace"))?
        .quantity
        .parse()?;
    eprintln!("✓ old order UTxO found at {}", old_order_utxo.address);
    eprintln!("  amount: {:?}", old_order_utxo.amount);

    // 2. Fetch pool, compute new min_receive.
    let pool = dex.liquidity_pool_from_pool_id(&cfg.pool_lp_token_unit).await
        .context("fetching pool from Kupo (is Kupo indexing the V2 validity asset?)")?
        .ok_or_else(|| anyhow!(
            "no pool found for LP token unit `{}`. Verify (a) the unit is correct and \
             (b) Kupo is indexing the Minswap V2 validity asset.",
            cfg.pool_lp_token_unit
        ))?;

    eprintln!("pool fetched from Kupo:");
    eprintln!("  assets:   {} / {}", token_unit(&pool.asset_a), token_unit(&pool.asset_b));
    eprintln!("  reserves: {} / {}", pool.reserve_a, pool.reserve_b);
    eprintln!("  fee:      {}%", pool.pool_fee_percent);

    let swap_in = token_from_unit(&cfg.swap_in_unit)?;
    let in_id = token_identifier(&swap_in);
    let a_id = token_identifier(&pool.asset_a);
    let b_id = token_identifier(&pool.asset_b);
    if in_id != a_id && in_id != b_id {
        bail!(
            "SWAP_IN_UNIT `{}` is not in the pool. Pool assets: `{}` / `{}`.",
            cfg.swap_in_unit, a_id, b_id
        );
    }

    let estimated = dex.estimated_receive(&pool, &swap_in, cfg.swap_in_amount);
    let multiplier = 1.0 + cfg.limit_premium_percent / 100.0;
    let min_receive = (estimated as f64 * multiplier).floor() as u64;

    eprintln!();
    eprintln!("new order pricing:");
    eprintln!("  current estimate:     {} raw out", estimated);
    eprintln!("  limit premium:        +{}%", cfg.limit_premium_percent);
    eprintln!("  computed min_receive: {}", min_receive);
    eprintln!();

    // 3. Build via UpdateSwapRequest — single PayToAddress combining:
    //    - p.address / p.assets / p.datum  → new order output (inline datum)
    //    - p.spend_utxos[0]               → old order spend (cancel redeemer + ref script)
    let pays = UpdateSwapRequest::new(&dex)
        .with_old_order_utxos(vec![old_order_utxo.clone()])
        .for_pool(pool)
        .with_swap_in_token(swap_in)
        .with_swap_in_amount(cfg.swap_in_amount)
        .with_minimum_receive(min_receive)
        .with_sender_address(&cfg.sender_bech32)?
        .build()?;
    let p = &pays[0];

    let datum_hex = p.datum.as_deref()
        .ok_or_else(|| anyhow!("library produced no datum for new order"))?;
    let datum_bytes = hex::decode(datum_hex).context("decoding datum cbor hex")?;
    let new_order_lovelace: u64 = p.assets
        .iter()
        .find(|a| a.unit == "lovelace")
        .ok_or_else(|| anyhow!("new order output missing lovelace"))?
        .quantity;

    let spend = &p.spend_utxos[0];
    let script_cbor = hex::decode(
        &spend.validator.as_ref()
            .ok_or_else(|| anyhow!("library missing validator"))?
            .cbor_hex
    ).context("decoding script cbor")?;
    let redeemer_cbor = hex::decode(
        spend.redeemer.as_ref().ok_or_else(|| anyhow!("library missing redeemer"))?,
    )?;

    // Prefer the reference script (CIP-33): Minswap V2 on mainnet always has it.
    // This keeps tx size at ~1KB instead of ~3.8KB and cuts fee accordingly.
    let validator_reference: Option<Input> = spend.validator_reference
        .as_ref()
        .map(|r| -> Result<_> {
            Ok(Input::new(parse_hash32(&r.tx_hash)?, r.output_index))
        })
        .transpose()?;
    match &spend.validator_reference {
        Some(r) => eprintln!(
            "✓ using reference script UTxO {}#{} (saves ~2.6KB in witness set)",
            r.tx_hash, r.output_index
        ),
        None => eprintln!("⚠ no validator_reference; using inline script (2.6KB in witness set)"),
    }

    eprintln!("new order address:  {}", p.address);
    eprintln!(
        "new order lovelace: {} ({:.6} ADA)",
        new_order_lovelace,
        new_order_lovelace as f64 / 1_000_000.0
    );
    eprintln!(
        "datum ({} bytes, inline): {}",
        datum_bytes.len(),
        datum_hex
    );
    eprintln!();

    // 4. Collateral UTxO.
    const MIN_COLLATERAL_LOVELACE: u64 = 2_000_000;
    let collateral = if let Some((c_tx, c_idx)) = &cfg.collateral_utxo {
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
            bail!(
                "COLLATERAL_UTXO {} must be pure ADA, but holds: {:?}",
                pattern, u.amount
            );
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
        (u, qty)
    } else {
        let wallet_utxos = kupo.get(&cfg.sender_bech32, true).await
            .context("fetching wallet UTxOs from Kupo")?;
        wallet_utxos.iter()
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
            ))?
    };
    eprintln!("✓ collateral: {}@{} ({} lovelace)",
        collateral.0.output_index, collateral.0.tx_hash, collateral.1);

    // 5. Wallet UTxO for fee.  We need a pure-ADA wallet input to cover the tx
    //    fee so the new order can receive the exact amount the library specifies.
    //    (Balance model: old_order_lovelace → new order; wallet_input → fee + change.)
    let wallet_utxos = kupo.get(&cfg.sender_bech32, true).await
        .context("fetching wallet UTxOs from Kupo")?;
    eprintln!("kupo returned {} UTxO(s) at sender", wallet_utxos.len());

    if let Some((c_tx, c_idx)) = &cfg.collateral_utxo {
        eprintln!("✓ reserving COLLATERAL_UTXO {}#{} from fee coin selection", c_tx, c_idx);
    }

    // Fee: taken from wallet input (pure-ADA), NOT from the new order's lovelace.
    // This keeps new_order_lovelace identical to what the library computed (5 ADA).
    let fee_estimate_initial: u64 = 300_000; // generous initial estimate for coin selection
    let min_change: u64 = 1_500_000;
    let target_wallet = fee_estimate_initial + min_change;

    let mut fee_utxo_candidates: Vec<(String, u64, u64)> = vec![];
    for u in &wallet_utxos {
        if u.amount.len() != 1 || u.amount[0].unit != "lovelace" {
            continue;
        }
        // Exclude pinned collateral from fee selection.
        if let Some((c_tx, c_idx)) = &cfg.collateral_utxo {
            if u.tx_hash == *c_tx && u.output_index as u64 == *c_idx {
                continue;
            }
        }
        let qty: u64 = u.amount[0].quantity.parse().unwrap_or(0);
        fee_utxo_candidates.push((u.tx_hash.clone(), u.output_index as u64, qty));
    }
    fee_utxo_candidates.sort_by(|a, b| b.2.cmp(&a.2));

    // Pick just one wallet input (the largest) if it covers fee + change.
    let fee_input = fee_utxo_candidates.first()
        .ok_or_else(|| anyhow!(
            "no pure-ADA wallet UTxOs available for fee. \
             Top up the wallet or ensure COLLATERAL_UTXO is not your only UTxO."
        ))?;
    if fee_input.2 < target_wallet {
        bail!(
            "largest wallet UTxO ({} lovelace) is too small for fee ({}) + min_change ({}).",
            fee_input.2, fee_estimate_initial, min_change
        );
    }
    eprintln!("✓ fee wallet input: {}#{} ({} lovelace)",
        fee_input.0, fee_input.1, fee_input.2);
    eprintln!();

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
    eprintln!("protocol: a={} b={} price_mem={}/{} price_step={}/{} v2_cost_entries={}",
        min_fee_a, min_fee_b, price_mem.0, price_mem.1, price_step.0, price_step.1, cost_v2.len());

    let current_slot = kupo.tip_slot().await.context("tip slot")?;
    let ttl = current_slot + 7200;
    eprintln!("slot: current={} ttl={}", current_slot, ttl);
    eprintln!();

    // 7. Eval pass: placeholder ex_units + placeholder fee.
    let order_script_input = Input::new(parse_hash32(&old_tx_hash)?, out_index);
    let fee_wallet_input = Input::new(parse_hash32(&fee_input.0)?, fee_input.1);
    let collateral_input = Input::new(
        parse_hash32(&collateral.0.tx_hash)?,
        collateral.0.output_index as u64,
    );
    let new_order_addr = PallasAddress::from_bech32(&p.address)
        .map_err(|e| anyhow!("parsing order script address: {}", e))?;
    let sender_addr = PallasAddress::from_bech32(&cfg.sender_bech32)
        .map_err(|e| anyhow!("parsing sender address: {}", e))?;

    // Build new order output: carry any non-lovelace assets from the library spec.
    // (The library puts the swap-in token on the order output when swapping a native token.)
    let new_order_extra_assets: Vec<(String, u64)> = p.assets.iter()
        .filter(|a| a.unit != "lovelace")
        .map(|a| (a.unit.clone(), a.quantity))
        .collect();

    let placeholder_ex = ExUnits { mem: 14_000_000, steps: 10_000_000_000 };
    let eval_fee: u64 = 1_000_000;
    let eval_change = fee_input.2 - eval_fee;

    let tx_for_eval = build_update_tx(
        order_script_input.clone(),
        fee_wallet_input.clone(),
        collateral_input.clone(),
        &old_order_utxo,
        new_order_addr.clone(),
        new_order_lovelace,
        &new_order_extra_assets,
        sender_addr.clone(),
        eval_change,
        datum_bytes.clone(),
        &script_cbor,
        validator_reference.clone(),
        &redeemer_cbor,
        placeholder_ex,
        Hash::<28>::from(payment_pkh),
        cost_v2.clone(),
        eval_fee,
        ttl,
    )?;

    let unsigned_built = tx_for_eval.build_conway_raw()
        .map_err(|e| anyhow!("initial build for eval: {:?}", e))?;
    eprintln!("evaluating script via Blockfrost...");
    let real_ex = blockfrost_evaluate(&client, &cfg.bf_project_id, &unsigned_built.tx_bytes.0).await
        .context("Blockfrost /utils/txs/evaluate")?;
    eprintln!("✓ script costs: mem={} steps={}", real_ex.mem, real_ex.steps);

    // 8. Compute script fee + iterative refinement.
    let script_fee = ceil_div(real_ex.mem as u128 * price_mem.0 as u128, price_mem.1 as u128)
        + ceil_div(real_ex.steps as u128 * price_step.0 as u128, price_step.1 as u128);
    let script_fee = script_fee as u64;
    eprintln!("script fee: {} lovelace", script_fee);

    const VKEY_WITNESS_OVERHEAD: u64 = 220;
    const FEE_SAFETY: u64 = 500;
    const MAX_ITERS: usize = 6;

    let mut fee_estimate: u64 = 200_000;
    let mut built = None;
    let mut converged = false;

    for iter in 0..MAX_ITERS {
        if fee_input.2 < fee_estimate + min_change {
            bail!(
                "wallet UTxO ({} lovelace) cannot cover fee ({}) + min_change ({})",
                fee_input.2, fee_estimate, min_change
            );
        }
        let change = fee_input.2 - fee_estimate;
        let tx = build_update_tx(
            order_script_input.clone(),
            fee_wallet_input.clone(),
            collateral_input.clone(),
            &old_order_utxo,
            new_order_addr.clone(),
            new_order_lovelace,
            &new_order_extra_assets,
            sender_addr.clone(),
            change,
            datum_bytes.clone(),
            &script_cbor,
            validator_reference.clone(),
            &redeemer_cbor,
            real_ex.clone(),
            Hash::<28>::from(payment_pkh),
            cost_v2.clone(),
            fee_estimate,
            ttl,
        )?;
        let raw = tx.build_conway_raw()
            .map_err(|e| anyhow!("build pass {}: {:?}", iter, e))?;
        let signed_size = raw.tx_bytes.0.len() as u64 + VKEY_WITNESS_OVERHEAD;
        let tx_fee = min_fee_b + min_fee_a * signed_size;
        let required = tx_fee + script_fee + FEE_SAFETY;
        eprintln!(
            "fee iter {}: estimate={} tx_fee={} script_fee={} required={} size~{}",
            iter, fee_estimate, tx_fee, script_fee, required, signed_size
        );
        if fee_estimate >= required {
            built = Some(raw);
            converged = true;
            break;
        }
        fee_estimate = required;
    }
    if !converged {
        bail!("fee refinement did not converge");
    }
    let built = built.unwrap();

    // Collateral coverage check.
    let required_collateral = (fee_estimate * collateral_percent + 99) / 100;
    if collateral.1 < required_collateral {
        bail!(
            "collateral UTxO ({} lovelace) below required {} ({}% of fee {})",
            collateral.1, required_collateral, collateral_percent, fee_estimate
        );
    }

    // 9. Sign.
    let signed = built.sign(&signing_key)
        .map_err(|e| anyhow!("sign: {:?}", e))?;
    let tx_cbor = hex::encode(&signed.tx_bytes.0);
    let tx_hash_out = hex::encode(signed.tx_hash.0);

    eprintln!();
    eprintln!("=== Signed update tx ready ===");
    eprintln!("tx hash: {}", tx_hash_out);
    eprintln!("tx size: {} bytes", signed.tx_bytes.0.len());
    eprintln!("fee:     {} lovelace ({:.6} ADA)", fee_estimate, fee_estimate as f64 / 1_000_000.0);
    eprintln!(
        "old ADA: {:.6} ADA  →  new ADA: {:.6} ADA (fee paid from wallet input)",
        old_order_lovelace as f64 / 1_000_000.0,
        new_order_lovelace as f64 / 1_000_000.0
    );
    eprintln!("tx cbor: {}", tx_cbor);
    eprintln!("inspect: {}/{}", CARDANOSCAN_TX, tx_hash_out);
    eprintln!();

    if !submit {
        eprintln!("DRY-RUN complete. Not broadcasting.");
        eprintln!("Re-run with --submit to broadcast.");
        return Ok(());
    }

    // 10. Submit.
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

/// Build a StagingTransaction for the update:
///   - old order input (script) with cancel redeemer + reference script (or inline)
///   - wallet input (fee source)
///   - collateral input
///   - new order output (inline datum) + change output
///   - required signer (payment PKH) so cancel script's signer check passes
#[allow(clippy::too_many_arguments)]
fn build_update_tx(
    order_input: Input,
    wallet_input: Input,
    collateral_input: Input,
    _old_order_utxo: &dexter_kupo_rs::models::Utxo,
    new_order_addr: PallasAddress,
    new_order_lovelace: u64,
    new_order_extra_assets: &[(String, u64)],
    sender_addr: PallasAddress,
    change_lovelace: u64,
    datum_bytes: Vec<u8>,
    script_cbor: &[u8],
    validator_reference: Option<Input>,
    redeemer_cbor: &[u8],
    ex_units: ExUnits,
    payment_pkh: Hash<28>,
    plutus_v2_cost_model: Vec<i64>,
    fee: u64,
    ttl: u64,
) -> Result<StagingTransaction> {
    // Build the new order output with inline datum.
    let mut new_order_out = Output::new(new_order_addr, new_order_lovelace)
        .set_inline_datum(datum_bytes);
    for (unit, qty) in new_order_extra_assets {
        if unit.len() < 56 {
            bail!("malformed asset unit on new order: {}", unit);
        }
        let (policy_hex, name_hex) = unit.split_at(56);
        let policy: [u8; 28] = hex::decode(policy_hex)?
            .try_into()
            .map_err(|_| anyhow!("policy id not 28 bytes"))?;
        let name: Vec<u8> = hex::decode(name_hex)?;
        new_order_out = new_order_out
            .add_asset(Hash::<28>::from(policy), name, *qty)
            .map_err(|e| anyhow!("add_asset to new order: {:?}", e))?;
    }

    // The old order's non-lovelace assets (if any) are NOT returned to the sender
    // in the update tx: they are consumed by the new order. But if the old order
    // had additional native tokens that are NOT part of the new swap, they need
    // to be returned via the change output. For Minswap V2 ADA→TOKEN and
    // TOKEN→ADA orders the order UTxO is always pure-ADA or holds exactly the
    // swap-in token — both of which are accounted for above. If you need to
    // handle more complex cases, extend this section.

    // Change output: wallet_input lovelace minus fee.
    let change_out = Output::new(sender_addr, change_lovelace);

    let mut language_map = BTreeMap::new();
    language_map.insert(1u8, plutus_v2_cost_model);
    let language_views = LanguageViews(language_map);

    let mut tx = StagingTransaction::new()
        .input(order_input.clone())
        .input(wallet_input)
        .collateral_input(collateral_input)
        .output(new_order_out)
        .output(change_out);

    if let Some(ref_input) = validator_reference {
        tx = tx.reference_input(ref_input);
    } else {
        tx = tx.script(ScriptKind::PlutusV2, script_cbor.to_vec());
    }

    let tx = tx
        .add_spend_redeemer(order_input, redeemer_cbor.to_vec(), Some(ex_units))
        .disclosed_signer(payment_pkh)
        .language_views(language_views)
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

/// POST raw tx CBOR to /utils/txs/evaluate, parse first spend redeemer's ex_units.
///
/// Blockfrost response shapes (tries both Ogmios 5 and Ogmios 6 formats):
///   Old: { "result": { "EvaluationResult": { "spend:0": {"memory":N,"steps":M} } } }
///   New: { "result": [ { "validator": {"purpose":"spend","index":0}, "budget": {"memory":N,"cpu":M} } ] }
async fn blockfrost_evaluate(client: &reqwest::Client, project_id: &str, tx_bytes: &[u8])
    -> Result<ExUnits>
{
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

    if let Some(map) = v["result"]["EvaluationResult"].as_object() {
        for (k, val) in map {
            if k.starts_with("spend:") {
                let mem = val["memory"].as_u64().ok_or_else(|| anyhow!("memory missing"))?;
                let steps = val["steps"].as_u64().ok_or_else(|| anyhow!("steps missing"))?;
                return Ok(ExUnits { mem, steps });
            }
        }
    }
    if let Some(arr) = v["result"].as_array() {
        for entry in arr {
            if entry["validator"]["purpose"].as_str() == Some("spend") {
                let mem = entry["budget"]["memory"].as_u64()
                    .ok_or_else(|| anyhow!("budget.memory missing"))?;
                let steps = entry["budget"]["cpu"].as_u64()
                    .or_else(|| entry["budget"]["steps"].as_u64())
                    .ok_or_else(|| anyhow!("budget.cpu/steps missing"))?;
                return Ok(ExUnits { mem, steps });
            }
        }
    }
    bail!("could not find spend redeemer ex_units in evaluate response: {}", body)
}

// ---------- helpers ----------

fn parse_flag(args: &[String], flag: &str) -> Result<String> {
    let i = args.iter().position(|a| a == flag)
        .ok_or_else(|| anyhow!("flag {} not found", flag))?;
    args.get(i + 1).cloned()
        .ok_or_else(|| anyhow!("flag {} missing value", flag))
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
        bail!(
            "token unit '{}' is too short (need at least 56 hex chars for policy ID)",
            unit
        );
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
    pool_lp_token_unit: String,
    swap_in_unit: String,
    swap_in_amount: u64,
    limit_premium_percent: f64,
    collateral_utxo: Option<(String, u64)>,
}

impl Config {
    fn from_env() -> Result<Self> {
        const REQUIRED: &[&str] = &[
            "KUPO_URL", "BLOCKFROST_PROJECT_ID", "SENDER_BECH32", "SENDER_MNEMONIC",
            "POOL_LP_TOKEN_UNIT", "SWAP_IN_UNIT", "SWAP_IN_AMOUNT",
        ];
        let missing: Vec<&str> = REQUIRED.iter().copied()
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

        Ok(Self {
            kupo_url, bf_project_id, sender_bech32,
            sender_mnemonic: env::var("SENDER_MNEMONIC").unwrap(),
            sender_mnemonic_passphrase: env::var("SENDER_MNEMONIC_PASSPHRASE").unwrap_or_default(),
            pool_lp_token_unit: env::var("POOL_LP_TOKEN_UNIT").unwrap(),
            swap_in_unit: env::var("SWAP_IN_UNIT").unwrap(),
            swap_in_amount: env::var("SWAP_IN_AMOUNT")
                .unwrap()
                .parse::<u64>()
                .context("SWAP_IN_AMOUNT must be a u64")?,
            limit_premium_percent,
            collateral_utxo,
        })
    }
}

// ---------- key derivation (same as live_cancel.rs) ----------

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
