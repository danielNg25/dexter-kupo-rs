/// Minswap Stable Pool — Curve-style stable swap implementation.
///
/// Unlike regular AMM DEXes, MinswapStable:
///   - Has NO pool discovery (cannot enumerate all pools)
///   - Requires explicit pool address + asset identifiers + decimals
///   - Reads reserves from the datum (Balance0/Balance1), NOT from UTXO amounts
///   - Uses a fixed fee of 0.1%
///
/// Datum structure (Plutus, Constr(0, [...])):
///   [0]: Array([Balance0, Balance1])  — reserve balances list
///   [1]: int — TotalLiquidity (D invariant)
///   [2]: int — AmplificationCoefficient (A)
///   [3]: bytes — OrderHash (ignored)
///
/// CLI usage:
///   cargo run --release -- --dex minswap_stable <pool_address> <asset_a> <asset_b> [decimals_a] [decimals_b]
use anyhow::{anyhow, Result};
use ciborium::value::Value;

use crate::kupo::KupoApi;
use crate::models::asset::from_identifier;
use crate::models::{StablePool, Utxo};
use super::cbor::{constr_fields, decode_cbor, value_to_u64};

const IDENTIFIER: &str = "MinswapStable";
const POOL_FEE_PERCENT: f64 = 0.1;

pub struct MinswapStable {
    kupo: KupoApi,
}

impl MinswapStable {
    pub fn new(kupo: KupoApi) -> Self {
        Self { kupo }
    }

    pub fn identifier(&self) -> &str {
        IDENTIFIER
    }

    /// Fetch and build a StablePool by its on-chain address.
    ///
    /// - `pool_address` — the bech32 pool address (acts as pool_id)
    /// - `asset_a_id`   — hex identifier for asset A (or "lovelace")
    /// - `asset_b_id`   — hex identifier for asset B
    /// - `decimals_a`   — decimal places for asset A (typically 6)
    /// - `decimals_b`   — decimal places for asset B (typically 6)
    pub async fn get_pool(
        &self,
        pool_address: &str,
        asset_a_id: &str,
        asset_b_id: &str,
        decimals_a: u8,
        decimals_b: u8,
    ) -> Result<StablePool> {
        let utxos = self.kupo.get(pool_address, true).await?;
        let utxo = utxos
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("No UTXOs found at pool address: {}", pool_address))?;

        self.pool_from_utxo(&utxo, asset_a_id, asset_b_id, decimals_a, decimals_b, pool_address)
            .await?
            .ok_or_else(|| anyhow!("Could not build stable pool from UTXO at {}", pool_address))
    }

    async fn pool_from_utxo(
        &self,
        utxo: &Utxo,
        asset_a_id: &str,
        asset_b_id: &str,
        decimals_a: u8,
        decimals_b: u8,
        pool_id: &str,
    ) -> Result<Option<StablePool>> {
        let data_hash = match &utxo.data_hash {
            Some(h) => h.clone(),
            None => return Ok(None),
        };

        let datum_cbor = self.kupo.datum(&data_hash).await?;
        let datum = parse_stable_datum(&datum_cbor).map_err(|e| {
            anyhow!("Failed parsing stable datum at {}: {}", utxo.address, e)
        })?;

        let asset_a = from_identifier(asset_a_id, decimals_a);
        let asset_b = from_identifier(asset_b_id, decimals_b);

        Ok(Some(StablePool {
            dex_identifier: IDENTIFIER.to_string(),
            asset_a,
            asset_b,
            reserve_a: datum.balance_0,
            reserve_b: datum.balance_1,
            address: utxo.address.clone(),
            pool_id: pool_id.to_string(),
            pool_fee_percent: POOL_FEE_PERCENT,
            amplification_coefficient: datum.amplification,
            total_liquidity: datum.total_liquidity,
        }))
    }
}

// ── Datum parsing ─────────────────────────────────────────────────────────────

struct StableDatum {
    balance_0: u64,
    balance_1: u64,
    total_liquidity: u64,
    amplification: u64,
}

/// Parse a Minswap Stable pool datum.
///
/// Raw CBOR example:
///   d879 9f
///     9f 1b000020cfb36eb471 1b0000002c0e972526 ff   ← balances array
///     1b00002c4eb6f0429b                            ← TotalLiquidity
///     0a                                            ← AmplificationCoefficient (10)
///     581c 2aa1ae...fc0ed                           ← OrderHash (bytes, ignored)
///   ff
fn parse_stable_datum(cbor_hex: &str) -> Result<StableDatum> {
    let value = decode_cbor(cbor_hex)?;

    // Outer: Constr(0, [...])
    let fields = constr_fields(&value)?;
    if fields.len() < 3 {
        return Err(anyhow!(
            "MinswapStable datum: expected >=3 fields, got {}",
            fields.len()
        ));
    }

    // fields[0]: plain Array([Balance0, Balance1]) — NOT a constructor
    let balances = match &fields[0] {
        Value::Array(arr) => arr,
        other => {
            return Err(anyhow!(
                "MinswapStable datum: expected array at field[0], got {:?}",
                other
            ))
        }
    };
    if balances.len() < 2 {
        return Err(anyhow!(
            "MinswapStable datum: balances array has {} elements, expected >=2",
            balances.len()
        ));
    }
    let balance_0 = value_to_u64(&balances[0])?;
    let balance_1 = value_to_u64(&balances[1])?;

    // fields[1]: TotalLiquidity
    let total_liquidity = value_to_u64(&fields[1])?;

    // fields[2]: AmplificationCoefficient
    let amplification = value_to_u64(&fields[2])?;

    // fields[3]: OrderHash bytes — ignored

    Ok(StableDatum {
        balance_0,
        balance_1,
        total_liquidity,
        amplification,
    })
}
