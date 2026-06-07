//! Live Minswap V2 order CANCEL example — spends a previously-placed order
//! UTxO using the cancel redeemer, returning everything to the original sender.
//!
//! USAGE:
//!   cargo run --example live_cancel -- --tx <hash>             # dry-run
//!   cargo run --example live_cancel -- --tx <hash> --submit    # broadcast
//!
//! ARGS:
//!   --tx <hash>          required: hash of the swap-placement tx
//!   --out-index <n>      optional: index of the order output (default 0)
//!   --submit             optional: actually broadcast (default: dry-run)
//!
//! Same env vars as live_swap.rs (KUPO_URL, BLOCKFROST_PROJECT_ID,
//! SENDER_BECH32, SENDER_MNEMONIC, plus optional SENDER_MNEMONIC_PASSPHRASE).
//! POOL_LP_TOKEN_UNIT, SWAP_IN_UNIT, SWAP_IN_AMOUNT, LIMIT_PREMIUM_PERCENT
//! are NOT used here — cancel doesn't care about pool details, just the UTxO.
//!
//! OPTIONAL ENV VARS:
//!   COLLATERAL_UTXO   Pin a specific UTxO as collateral, format `<tx_hash>#<idx>`
//!                     (e.g., `COLLATERAL_UTXO=abc123...#0`).
//!                     If unset, the largest pure-ADA UTxO in the wallet is used.
//!                     RECOMMENDED FOR PRODUCTION: keep one ~10 ADA pure-ADA UTxO
//!                     set aside and never spend it. Cancels don't consume
//!                     collateral on success, so it persists across thousands of txs
//!                     and prevents wallet fragmentation from breaking collateral lookup.
//!
//! KUPO requirements:
//!   - Indexing the V2 order script address (or `*`) so the order UTxO is
//!     findable. Start Kupo with:
//!       --match addr1z8p79rpkcdz8x9d6tft0x0dx5mwuzac2sa4gm8cvkw5hcn8ftv3526r2y8z8rnlvay76nhpdp9pdekzvd3rrrql08qqssmhdxz
//!     (your order's address — the part after `addr1z` is the same for everyone)
//!     OR `--match "*"` for everything.
//!   - Indexing SENDER_BECH32 (for collateral UTxO lookup).

use anyhow::{anyhow, bail, Context, Result};
use bip39::{Language, Mnemonic};
use dexter_kupo_rs::{
    dex::minswap_v2::MinswapV2,
    CancelSwapRequest, KupoApi,
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
    let tx_hash = parse_flag(&args, "--tx")
        .context("--tx <hash> is required")?;
    let out_index: u64 = parse_flag(&args, "--out-index")
        .unwrap_or_else(|_| "0".to_string())
        .parse()
        .context("--out-index must be a u64")?;

    let cfg = Config::from_env().context("loading env vars")?;

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

    // Payment key hash (Blake2b-224 of pubkey) — needed for required_signers.
    let payment_pub: [u8; 32] = payment_xprv.public().public_key();
    let payment_pkh: [u8; 28] = {
        let h = pallas_crypto::hash::Hasher::<224>::hash(&payment_pub);
        let mut out = [0u8; 28];
        out.copy_from_slice(h.as_ref());
        out
    };

    let client = reqwest::Client::new();

    eprintln!("== Live Cancel (Minswap V2) ==");
    eprintln!("mode:   {}", if submit { "SUBMIT" } else { "DRY-RUN" });
    eprintln!("order:  {}@{}", out_index, tx_hash);
    eprintln!("sender: {}", cfg.sender_bech32);
    eprintln!();

    let kupo = KupoApi::new(&cfg.kupo_url);
    let dex = MinswapV2::new(KupoApi::new(&cfg.kupo_url));

    // 1. Look up the order UTxO. Kupo accepts `{output_index}@{tx_hash}` as a query pattern.
    let order_ref = format!("{}@{}", out_index, tx_hash);
    let order_utxos = kupo.get(&order_ref, true).await
        .context("fetching order UTxO from Kupo")?;
    if order_utxos.is_empty() {
        bail!(
            "order UTxO {} not found at Kupo. Either (a) the UTxO was already spent (filled or cancelled), or (b) Kupo is not indexing the order script address. Add `--match <order_script_addr>` to Kupo or use `--match \"*\"`.",
            order_ref
        );
    }
    let order_utxo = order_utxos[0].clone();
    eprintln!("✓ order UTxO found at {}", order_utxo.address);
    eprintln!("  amount: {:?}", order_utxo.amount);

    let order_lovelace: u64 = order_utxo
        .amount
        .iter()
        .find(|a| a.unit == "lovelace")
        .ok_or_else(|| anyhow!("order UTxO has no lovelace"))?
        .quantity
        .parse()?;

    // 2. Build the cancel request via the library.
    let pays = CancelSwapRequest::new(&dex)
        .with_order_utxos(vec![order_utxo.clone()])
        .with_return_address(&cfg.sender_bech32)
        .build()?;
    let p = &pays[0];
    let spend = &p.spend_utxos[0];
    let script_cbor = hex::decode(&spend.validator.as_ref()
        .ok_or_else(|| anyhow!("library missing validator"))?
        .cbor_hex)
        .context("decoding script cbor")?;
    let redeemer_cbor = hex::decode(
        spend.redeemer.as_ref().ok_or_else(|| anyhow!("library missing redeemer"))?,
    )?;
    eprintln!("✓ cancel built: script {} bytes, redeemer {} bytes",
        script_cbor.len(), redeemer_cbor.len());

    // Prefer validator_reference (reference script UTxO) when the library provides it.
    // On mainnet Minswap V2, this is the published UTxO that carries the 2659-byte script,
    // saving it from the witness set: tx size drops from ~3.8KB to ~1KB.
    let validator_reference: Option<Input> = spend.validator_reference
        .as_ref()
        .map(|r| -> Result<_> {
            Ok(Input::new(parse_hash32(&r.tx_hash)?, r.output_index))
        })
        .transpose()?;
    match &spend.validator_reference {
        Some(r) => eprintln!(
            "✓ using reference script UTxO {}#{} (saves ~2.6KB in tx witness set)",
            r.tx_hash, r.output_index
        ),
        None => eprintln!("⚠ no validator_reference; using inline script (2.6KB in witness set)"),
    }

    // 3. Resolve the collateral UTxO.
    //    Required size = collateral_percent (150%) × tx_fee (~0.5 ADA) = ~0.75 ADA,
    //    plus Cardano's min-UTxO floor (~1 ADA). 2 ADA is enough headroom.
    const MIN_COLLATERAL_LOVELACE: u64 = 2_000_000;
    let collateral = if let Some((c_tx, c_idx)) = &cfg.collateral_utxo {
        // Pinned mode: look up the exact UTxO and validate it.
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
                "COLLATERAL_UTXO {} is at address {}, not at SENDER_BECH32 {}",
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
            .map_err(|_| anyhow!("COLLATERAL_UTXO {} quantity not u64", pattern))?;
        if qty < MIN_COLLATERAL_LOVELACE {
            bail!(
                "COLLATERAL_UTXO {} has only {} lovelace, need at least {}",
                pattern, qty, MIN_COLLATERAL_LOVELACE
            );
        }
        eprintln!("✓ using pinned COLLATERAL_UTXO {}", pattern);
        (u, qty)
    } else {
        // Auto mode: pick the largest pure-ADA UTxO in the wallet.
        // Note: cancelled orders return ~4 ADA UTxOs which fragment the wallet
        // quickly; if this fails, set COLLATERAL_UTXO to pin a specific UTxO.
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
                 Either top up the wallet, or set COLLATERAL_UTXO=<tx_hash>#<idx> \
                 to pin a specific UTxO.",
                MIN_COLLATERAL_LOVELACE
            ))?
    };
    eprintln!("✓ collateral: {}@{} ({} lovelace)",
        collateral.0.output_index, collateral.0.tx_hash, collateral.1);
    eprintln!();

    // 4. Fetch protocol params: fee coefficients, script-price coefficients,
    //    PlutusV2 cost model (for the script_data_hash).
    let params = blockfrost_get(&client, &cfg.bf_project_id, "/epochs/latest/parameters")
        .await
        .context("fetching protocol params")?;
    let min_fee_a = params["min_fee_a"].as_u64()
        .ok_or_else(|| anyhow!("min_fee_a missing"))?;
    let min_fee_b = params["min_fee_b"].as_u64()
        .ok_or_else(|| anyhow!("min_fee_b missing"))?;
    let price_mem = parse_price(&params["price_mem"])
        .context("parsing price_mem")?;
    let price_step = parse_price(&params["price_step"])
        .context("parsing price_step")?;
    let collateral_percent = params["collateral_percent"].as_u64().unwrap_or(150);
    // Conway-era Blockfrost: `cost_models_raw.PlutusV2` is the flat ordered array;
    // `cost_models.PlutusV2` is an object keyed by parameter name (unordered).
    // Older endpoints returned the array directly under `cost_models.PlutusV2`.
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
            "could not find PlutusV2 cost model array under `cost_models_raw.PlutusV2` or `cost_models.PlutusV2`. response keys: {:?}",
            params.as_object().map(|m| m.keys().collect::<Vec<_>>())
        );
    };
    eprintln!("protocol: a={} b={} price_mem={}/{} price_step={}/{} v2_cost_entries={}",
        min_fee_a, min_fee_b, price_mem.0, price_mem.1, price_step.0, price_step.1, cost_v2.len());

    let current_slot = kupo.tip_slot().await.context("tip slot")?;
    let ttl = current_slot + 7200;
    eprintln!("slot: current={} ttl={}", current_slot, ttl);
    eprintln!();

    // 5. Build pass with PLACEHOLDER ex_units, evaluate via Blockfrost.
    let order_input = Input::new(parse_hash32(&tx_hash)?, out_index);
    let collateral_input = Input::new(
        parse_hash32(&collateral.0.tx_hash)?,
        collateral.0.output_index as u64,
    );
    let return_addr = PallasAddress::from_bech32(&cfg.sender_bech32)
        .map_err(|e| anyhow!("parsing sender address: {}", e))?;
    let placeholder_ex = ExUnits { mem: 14_000_000, steps: 10_000_000_000 };

    // First we need a rough fee guess to make a balanced tx for evaluation.
    // 1 ADA is plenty for the eval-only pass; we'll refine after.
    let mut fee_estimate: u64 = 1_000_000;
    let return_lovelace = order_lovelace - fee_estimate;

    let tx_for_eval = build_cancel_tx(
        order_input,
        collateral_input,
        &order_utxo,
        return_addr.clone(),
        return_lovelace,
        &script_cbor,
        validator_reference.clone(),
        &redeemer_cbor,
        placeholder_ex,
        Hash::<28>::from(payment_pkh),
        cost_v2.clone(),
        fee_estimate,
        ttl,
    )?;
    let unsigned_built = tx_for_eval.build_conway_raw()
        .map_err(|e| anyhow!("initial build for evaluation: {:?}", e))?;
    eprintln!("evaluating script via Blockfrost...");
    let real_ex = blockfrost_evaluate(&client, &cfg.bf_project_id, &unsigned_built.tx_bytes.0).await
        .context("Blockfrost /utils/txs/evaluate")?;
    eprintln!("✓ script costs: mem={} steps={}", real_ex.mem, real_ex.steps);

    // 6. Compute script fee + iteratively refine total fee.
    let script_fee = ceil_div(real_ex.mem as u128 * price_mem.0 as u128, price_mem.1 as u128)
        + ceil_div(real_ex.steps as u128 * price_step.0 as u128, price_step.1 as u128);
    let script_fee = script_fee as u64;
    eprintln!("script fee: {} lovelace", script_fee);

    const VKEY_WITNESS_OVERHEAD: u64 = 220; // payment vkey + signature, plus we sign once
    const FEE_SAFETY: u64 = 500;
    const MAX_ITERS: usize = 6;

    // Reset to a small estimate so the fixpoint loop converges to the actual
    // minimum required fee instead of locking in the 1 ADA eval-pass placeholder.
    // 200_000 is well below any realistic Plutus cancel fee, so the first
    // iteration will compute `required` and we bump up from there.
    fee_estimate = 200_000;

    let mut built = None;
    let mut converged = false;
    for iter in 0..MAX_ITERS {
        if order_lovelace < fee_estimate + 1_000_000 {
            bail!("order UTxO ({}) cannot cover fee ({}) + min return", order_lovelace, fee_estimate);
        }
        let return_lovelace = order_lovelace - fee_estimate;
        let tx = build_cancel_tx(
            Input::new(parse_hash32(&tx_hash)?, out_index),
            Input::new(parse_hash32(&collateral.0.tx_hash)?, collateral.0.output_index as u64),
            &order_utxo,
            return_addr.clone(),
            return_lovelace,
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

    // Required collateral coverage check.
    let required_collateral = (fee_estimate * collateral_percent + 99) / 100;
    if collateral.1 < required_collateral {
        bail!(
            "collateral UTxO ({} lovelace) below required {} ({}% of fee {})",
            collateral.1, required_collateral, collateral_percent, fee_estimate
        );
    }

    // 7. Sign.
    let signed = built.sign(&signing_key)
        .map_err(|e| anyhow!("sign: {:?}", e))?;
    let tx_cbor = hex::encode(&signed.tx_bytes.0);
    let tx_hash_out = hex::encode(signed.tx_hash.0);
    eprintln!();
    eprintln!("=== Signed cancel tx ready ===");
    eprintln!("tx hash: {}", tx_hash_out);
    eprintln!("tx size: {} bytes", signed.tx_bytes.0.len());
    eprintln!("tx cbor: {}", tx_cbor);
    eprintln!("inspect: {}/{}", CARDANOSCAN_TX, tx_hash_out);
    eprintln!();

    if !submit {
        eprintln!("DRY-RUN complete. Not broadcasting.");
        eprintln!("Re-run with --submit to broadcast.");
        return Ok(());
    }

    // 8. Submit.
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

#[allow(clippy::too_many_arguments)]
fn build_cancel_tx(
    order_input: Input,
    collateral_input: Input,
    order_utxo: &dexter_kupo_rs::models::Utxo,
    return_addr: PallasAddress,
    return_lovelace: u64,
    script_cbor: &[u8],
    validator_reference: Option<Input>,
    redeemer_cbor: &[u8],
    ex_units: ExUnits,
    payment_pkh: Hash<28>,
    plutus_v2_cost_model: Vec<i64>,
    fee: u64,
    ttl: u64,
) -> Result<StagingTransaction> {
    // Return output: send back all of the order UTxO's assets to the sender,
    // minus the fee taken from lovelace.
    let mut out = Output::new(return_addr, return_lovelace);
    for unit in &order_utxo.amount {
        if unit.unit == "lovelace" {
            continue; // handled by Output::new
        }
        // Asset unit is "<policy_hex><name_hex>" with no dot.
        if unit.unit.len() < 56 {
            bail!("malformed asset unit on order UTxO: {}", unit.unit);
        }
        let (policy_hex, name_hex) = unit.unit.split_at(56);
        let policy: [u8; 28] = hex::decode(policy_hex)?
            .try_into()
            .map_err(|_| anyhow!("policy id not 28 bytes"))?;
        let name: Vec<u8> = hex::decode(name_hex)?;
        let qty: u64 = unit.quantity.parse()?;
        out = out.add_asset(Hash::<28>::from(policy), name, qty)
            .map_err(|e| anyhow!("add_asset: {:?}", e))?;
    }

    let mut language_map = BTreeMap::new();
    language_map.insert(1u8, plutus_v2_cost_model);
    let language_views = LanguageViews(language_map);

    let mut tx = StagingTransaction::new()
        .input(order_input.clone())
        .collateral_input(collateral_input)
        .output(out);
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
/// Blockfrost response (older Ogmios-style):
///   { "result": { "EvaluationResult": { "spend:0": {"memory": N, "steps": M} } } }
/// Or (newer Ogmios 6 shape):
///   { "result": [ { "validator": {"purpose":"spend","index":0}, "budget": {"memory":N,"cpu":M} } ] }
async fn blockfrost_evaluate(client: &reqwest::Client, project_id: &str, tx_bytes: &[u8])
    -> Result<ExUnits>
{
    let url = format!("{}/utils/txs/evaluate", BLOCKFROST_BASE);
    // Blockfrost expects hex-encoded CBOR as application/cbor body.
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

    // Try old shape first: result.EvaluationResult.{"spend:0": {memory, steps}}
    if let Some(map) = v["result"]["EvaluationResult"].as_object() {
        for (k, val) in map {
            if k.starts_with("spend:") {
                let mem = val["memory"].as_u64()
                    .ok_or_else(|| anyhow!("memory missing"))?;
                let steps = val["steps"].as_u64()
                    .ok_or_else(|| anyhow!("steps missing"))?;
                return Ok(ExUnits { mem, steps });
            }
        }
    }
    // Try newer shape: result is an array of { validator: {purpose,index}, budget: {memory,cpu} }
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

/// Parse Blockfrost price representation. Older API returns a string like
/// "0.0577", newer API returns a {numerator, denominator} object.
fn parse_price(v: &serde_json::Value) -> Result<(u64, u64)> {
    if let Some(s) = v.as_str() {
        // String form: "0.0577" -> (577, 10000)
        let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
        let denom: u64 = 10u64.pow(frac.len() as u32);
        let num: u64 = format!("{}{}", whole, frac).parse()?;
        return Ok((num, denom));
    }
    if let Some(n) = v.as_f64() {
        // Float form: convert with millionths precision.
        let denom = 1_000_000_000u64;
        let num = (n * denom as f64).round() as u64;
        return Ok((num, denom));
    }
    if let (Some(num), Some(denom)) = (
        v["numerator"].as_u64(),
        v["denominator"].as_u64(),
    ) {
        return Ok((num, denom));
    }
    bail!("could not parse price value: {:?}", v)
}

fn ceil_div(a: u128, b: u128) -> u128 {
    (a + b - 1) / b
}

// ---------- config ----------

struct Config {
    kupo_url: String,
    bf_project_id: String,
    sender_bech32: String,
    sender_mnemonic: String,
    sender_mnemonic_passphrase: String,
    /// Optional pinned collateral UTxO as (tx_hash, output_index).
    collateral_utxo: Option<(String, u64)>,
}

impl Config {
    fn from_env() -> Result<Self> {
        const REQUIRED: &[&str] = &[
            "KUPO_URL", "BLOCKFROST_PROJECT_ID", "SENDER_BECH32", "SENDER_MNEMONIC",
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
            bail!("SENDER_BECH32 must be mainnet (addr1...)");
        }
        if !bf_project_id.starts_with("mainnet") {
            bail!("BLOCKFROST_PROJECT_ID must start with `mainnet`");
        }
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
            collateral_utxo,
        })
    }
}

// ---------- key derivation (same as live_swap.rs) ----------

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
