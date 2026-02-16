/// VyFi Bar – staking/rate calculator for VyFi pools.
///
/// Unlike regular DEX implementations, VyFi Bar does NOT produce `LiquidityPool` objects.
/// It returns a `Rate { base_asset, derived_asset }` representing the exchange rate
/// between the staked token and the derived (xVYFI) token in the bar pool.
///
/// Datum structure (Plutus):
///   Constr(0, [
///     Constr(0, [
///       int  ← ReserveA (derived asset amount)
///     ])
///   ])
///
/// Usage:
///   - `get_rate(pool_identifier)` — fetch UTXOs for the pool identifier, parse rate
///   - `rate_from_utxo(utxo)` — parse rate from a single UTXO (fetches datum)
///
/// Pool identifier format: `<policy_id>.` (empty asset name, e.g. for VYFI/xVYFI pool).
use anyhow::{anyhow, Result};
use serde::Serialize;

use crate::kupo::KupoApi;
use crate::models::Utxo;
use super::cbor::{constr_fields, decode_cbor, value_to_u64};

const IDENTIFIER: &str = "VyfiBar";

/// Exchange rate for a VyFi Bar pool.
#[derive(Debug, Clone, Serialize)]
pub struct Rate {
    /// Identifier of the pool queried.
    pub pool_identifier: String,
    /// Amount of the base asset (non-lovelace, non-pool token from UTXO).
    pub base_asset: u64,
    /// Amount of the derived asset (ReserveA from datum).
    pub derived_asset: u64,
}

pub struct VyfiBar {
    kupo: KupoApi,
}

impl VyfiBar {
    pub fn new(kupo: KupoApi) -> Self {
        Self { kupo }
    }

    pub fn identifier(&self) -> &str {
        IDENTIFIER
    }

    /// Fetch UTXOs for the given pool identifier pattern (e.g. `"policy_id."`).
    pub async fn rate_utxos(&self, pool_identifier: &str) -> Result<Vec<Utxo>> {
        self.kupo.get(pool_identifier, true).await
    }

    /// Get the exchange rate for a pool by its identifier.
    ///
    /// Mirrors JS `getRate(poolIdentifier)`:
    ///   1. Fetch UTXOs for the pool identifier.
    ///   2. Take the first UTXO.
    ///   3. Find the base asset: non-lovelace token whose unit does NOT start with the policy ID.
    ///   4. Fetch datum and extract ReserveA as the derived asset amount.
    pub async fn get_rate(&self, pool_identifier: &str) -> Result<Rate> {
        let utxos = self.rate_utxos(pool_identifier).await?;
        let utxo = utxos
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("No UTXOs found for pool identifier: {}", pool_identifier))?;

        self.rate_from_utxo_with_id(&utxo, pool_identifier).await
    }

    /// Parse a Rate from a single UTXO, given its pool identifier.
    ///
    /// Returns `None` if the UTXO has no datum hash (can't parse rate).
    pub async fn rate_from_utxo(&self, utxo: &Utxo, pool_identifier: &str) -> Result<Option<Rate>> {
        if utxo.data_hash.is_none() {
            return Ok(None);
        }
        Ok(Some(self.rate_from_utxo_with_id(utxo, pool_identifier).await?))
    }

    async fn rate_from_utxo_with_id(&self, utxo: &Utxo, pool_identifier: &str) -> Result<Rate> {
        // The policy ID is everything before the first dot (or the full string if no dot).
        let policy_id = pool_identifier
            .split('.')
            .next()
            .unwrap_or(pool_identifier);

        // Find the base asset: non-lovelace token that does NOT belong to the pool policy.
        let base_unit = utxo
            .amount
            .iter()
            .find(|a| a.unit != "lovelace" && !a.unit.starts_with(policy_id))
            .ok_or_else(|| {
                anyhow!(
                    "VyfiBar: no base asset found in UTXO {} for pool {}",
                    utxo.tx_hash,
                    pool_identifier
                )
            })?;

        let base_asset = base_unit.quantity.parse::<u64>().map_err(|e| {
            anyhow!(
                "VyfiBar: failed to parse base asset quantity '{}': {}",
                base_unit.quantity,
                e
            )
        })?;

        // Fetch datum and parse ReserveA as the derived asset.
        let data_hash = utxo
            .data_hash
            .as_ref()
            .ok_or_else(|| anyhow!("VyfiBar: UTXO {} has no datum hash", utxo.tx_hash))?;

        let datum_cbor = self.kupo.datum(data_hash).await?;
        let derived_asset = parse_bar_datum(&datum_cbor)?;

        Ok(Rate {
            pool_identifier: pool_identifier.to_string(),
            base_asset,
            derived_asset,
        })
    }
}

/// Parse VyFi Bar pool datum.
///
/// Plutus structure:
///   Constr(0, [
///     Constr(0, [
///       int  ← ReserveA
///     ])
///   ])
fn parse_bar_datum(cbor_hex: &str) -> Result<u64> {
    let value = decode_cbor(cbor_hex)?;
    // Outer constructor
    let outer = constr_fields(&value)?;
    if outer.is_empty() {
        return Err(anyhow!("VyfiBar datum: outer constructor has no fields"));
    }
    // Inner constructor
    let inner = constr_fields(&outer[0])?;
    if inner.is_empty() {
        return Err(anyhow!("VyfiBar datum: inner constructor has no fields"));
    }
    // ReserveA
    value_to_u64(&inner[0]).map_err(|e| anyhow!("VyfiBar datum ReserveA: {}", e))
}
