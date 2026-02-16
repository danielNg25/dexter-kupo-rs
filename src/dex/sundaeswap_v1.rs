use anyhow::{anyhow, Result};
use async_trait::async_trait;
use crate::kupo::KupoApi;
use crate::models::asset::from_identifier;
use crate::models::{token_identifier, LiquidityPool, Utxo};
use super::BaseDex;
use super::cbor::{constr_fields, decode_cbor, value_to_u64};

const IDENTIFIER: &str = "SUNDAESWAPV1";
const POOL_ADDRESS: &str = "addr1w9qzpelu9hn45pefc0xr4ac4kdxeswq7pndul2vuj59u8tqaxdznu";
const LP_TOKEN_POLICY_ID: &str = "0029cb7c88c7567b63d1a512c0ed626aa169688ec980730c0473b913";

pub struct SundaeSwapV1 {
    kupo: KupoApi,
}

impl SundaeSwapV1 {
    pub fn new(kupo: KupoApi) -> Self {
        Self { kupo }
    }
}

/// Parse SundaeSwapV1 pool datum.
///
/// Structure (constructor 0):
///   [0]: constr — asset pair info (two sub-constrs for assetA/assetB), ignored
///   [1]: bytes  — pool identifier, ignored
///   [2]: int    — TotalLpTokens
///   [3]: constr — fee: { LpFeeNumerator, LpFeeDenominator }
fn parse_pool_datum(cbor_hex: &str) -> Result<(u64, u64, u64)> {
    let value = decode_cbor(cbor_hex)?;
    let fields = constr_fields(&value)?;

    if fields.len() < 4 {
        return Err(anyhow!("SundaeSwapV1 datum: expected >=4 fields, got {}", fields.len()));
    }

    // fields[2]: TotalLpTokens
    let total_lp = value_to_u64(&fields[2])?;

    // fields[3]: constr { numerator, denominator }
    let fee_fields = constr_fields(&fields[3])?;
    if fee_fields.len() < 2 {
        return Err(anyhow!("SundaeSwapV1 fee constr: expected 2 fields, got {}", fee_fields.len()));
    }
    let numerator = value_to_u64(&fee_fields[0])?;
    let denominator = value_to_u64(&fee_fields[1])?;

    Ok((total_lp, numerator, denominator))
}

#[async_trait]
impl BaseDex for SundaeSwapV1 {
    fn identifier(&self) -> &str {
        IDENTIFIER
    }

    fn pool_address(&self) -> &str {
        POOL_ADDRESS
    }

    fn lp_token_policy_id(&self) -> &str {
        LP_TOKEN_POLICY_ID
    }

    fn kupo(&self) -> &KupoApi {
        &self.kupo
    }

    async fn all_liquidity_pool_utxos(&self) -> Result<Vec<Utxo>> {
        // Kupo: bech32 addresses are queried directly (no /* wildcard)
        self.kupo.get(POOL_ADDRESS, true).await
    }

    /// Build a preliminary LiquidityPool from UTXO amounts.
    /// LP token assets are excluded and used to set pool_id.
    /// Default fee 0.3 — overridden in extend.
    async fn liquidity_pool_from_utxo(
        &self,
        utxo: &Utxo,
        pool_id: &str,
    ) -> Result<Option<LiquidityPool>> {
        if utxo.data_hash.is_none() {
            return Ok(None);
        }

        let mut pool_id = pool_id.to_string();

        let relevant: Vec<_> = utxo
            .amount
            .iter()
            .filter(|a| {
                if a.unit.starts_with(LP_TOKEN_POLICY_ID) {
                    pool_id = a.unit.clone();
                    false
                } else {
                    true
                }
            })
            .collect();

        if ![2, 3].contains(&relevant.len()) {
            return Ok(None);
        }

        let a_idx = if relevant.len() == 2 { 0 } else { 1 };
        let b_idx = if relevant.len() == 2 { 1 } else { 2 };

        let asset_a = from_identifier(&relevant[a_idx].unit, 0);
        let asset_b = from_identifier(&relevant[b_idx].unit, 0);
        let reserve_a = relevant[a_idx].quantity.parse::<u64>()?;
        let reserve_b = relevant[b_idx].quantity.parse::<u64>()?;

        Ok(Some(LiquidityPool::new(
            IDENTIFIER,
            asset_a,
            asset_b,
            reserve_a,
            reserve_b,
            &utxo.address,
            0.3,
            &pool_id,
        )))
    }

    /// Fetch datum and update fee.
    /// Fee = (LpFeeNumerator / LpFeeDenominator) * 100
    async fn liquidity_pool_from_utxo_extend(
        &self,
        utxo: &Utxo,
        pool_id: &str,
    ) -> Result<Option<LiquidityPool>> {
        let mut pool = match self.liquidity_pool_from_utxo(utxo, pool_id).await? {
            Some(p) => p,
            None => return Ok(None),
        };

        let data_hash = match &utxo.data_hash {
            Some(h) => h,
            None => return Ok(None),
        };

        let datum = self.kupo.datum(data_hash).await?;
        let (total_lp, numerator, denominator) = parse_pool_datum(&datum)?;

        pool.total_lp_tokens = total_lp;
        pool.pool_fee_percent = if denominator > 0 {
            (numerator as f64 / denominator as f64) * 100.0
        } else {
            0.3
        };

        Ok(Some(pool))
    }

    async fn liquidity_pool_from_pool_id(&self, pool_id: &str) -> Result<Option<LiquidityPool>> {
        let full_id = if pool_id.starts_with(LP_TOKEN_POLICY_ID) {
            pool_id.to_string()
        } else {
            format!("{}{}", LP_TOKEN_POLICY_ID, pool_id)
        };

        let utxos = self.all_liquidity_pool_utxos().await?;
        let found = utxos
            .iter()
            .find(|u| u.amount.iter().any(|a| a.unit == full_id));

        match found {
            Some(utxo) => self.liquidity_pool_from_utxo_extend(utxo, &full_id).await,
            None => Ok(None),
        }
    }

    async fn liquidity_pools_from_token(
        &self,
        token_b: &str,
        token_a: &str,
    ) -> Result<Vec<LiquidityPool>> {
        let all_utxos = self.all_liquidity_pool_utxos().await?;
        let mut pools = Vec::new();

        for utxo in &all_utxos {
            let base = match self.liquidity_pool_from_utxo(utxo, "").await? {
                Some(p) => p,
                None => continue,
            };

            let id_a = token_identifier(&base.asset_a);
            let id_b = token_identifier(&base.asset_b);

            let matches = (id_a == token_a && id_b == token_b)
                || (id_a == token_b && id_b == token_a);

            if matches {
                match self
                    .liquidity_pool_from_utxo_extend(utxo, &base.pool_id)
                    .await
                {
                    Ok(Some(p)) => pools.push(p),
                    Ok(None) => {}
                    Err(e) => eprintln!("[sundaeswap_v1] datum error {}: {}", utxo.tx_hash, e),
                }
            }
        }

        Ok(pools)
    }
}
