use anyhow::{anyhow, Result};
use async_trait::async_trait;
use crate::kupo::KupoApi;
use crate::models::asset::from_identifier;
use crate::models::{token_identifier, LiquidityPool, Utxo};
use super::BaseDex;
use super::cbor::{constr_fields, decode_cbor, value_to_u64, value_to_i64};

const IDENTIFIER: &str = "SUNDAESWAPV3";
// Two pool contract addresses — pools live at both
const POOL_ADDRESS_V1: &str =
    "addr1x8srqftqemf0mjlukfszd97ljuxdp44r372txfcr75wrz26rnxqnmtv3hdu2t6chcfhl2zzjh36a87nmd6dwsu3jenqsslnz7e";
const POOL_ADDRESS_V2: &str =
    "addr1z8srqftqemf0mjlukfszd97ljuxdp44r372txfcr75wrz2auzrlrz2kdd83wzt9u9n9qt2swgvhrmmn96k55nq6yuj4qw992w9";
const LP_TOKEN_POLICY_ID: &str =
    "e0302560ced2fdcbfcb2602697df970cd0d6a38f94b32703f51c312b";

pub struct SundaeSwapV3 {
    kupo: KupoApi,
}

impl SundaeSwapV3 {
    pub fn new(kupo: KupoApi) -> Self {
        Self { kupo }
    }
}

/// Parse SundaeSwapV3 pool datum.
///
/// Structure (constructor 0):
///   [0]: bytes — PoolIdentifier (ignored)
///   [1]: list  — [[policyA, nameA], [policyB, nameB]] (ignored)
///   [2]: int   — TotalLpTokens
///   [3]: int   — OpeningFee (ignored)
///   [4]: int   — FinalFee  → fee = FinalFee / 100
///   [5]: (skipped — function in JS)
///   [6]: int   — Unknown (ignored)
///   [7]: int   — LovelaceDeduction
struct V3Datum {
    total_lp: u64,
    final_fee: u64,
    lovelace_deduction: i64,
}

fn parse_pool_datum(cbor_hex: &str) -> Result<V3Datum> {
    let value = decode_cbor(cbor_hex)?;
    let fields = constr_fields(&value)?;

    if fields.len() < 8 {
        return Err(anyhow!(
            "SundaeSwapV3 datum: expected >=8 fields, got {}",
            fields.len()
        ));
    }

    // [2]: TotalLpTokens
    let total_lp = value_to_u64(&fields[2])?;
    // [4]: FinalFee
    let final_fee = value_to_u64(&fields[4])?;
    // [7]: LovelaceDeduction
    let lovelace_deduction = value_to_i64(&fields[7])?;

    Ok(V3Datum {
        total_lp,
        final_fee,
        lovelace_deduction,
    })
}

#[async_trait]
impl BaseDex for SundaeSwapV3 {
    fn identifier(&self) -> &str {
        IDENTIFIER
    }

    fn pool_address(&self) -> &str {
        POOL_ADDRESS_V1
    }

    fn lp_token_policy_id(&self) -> &str {
        LP_TOKEN_POLICY_ID
    }

    fn kupo(&self) -> &KupoApi {
        &self.kupo
    }

    /// Fetch UTXOs from both pool addresses and merge.
    /// Kupo: bech32 addresses are queried directly (no /* wildcard)
    async fn all_liquidity_pool_utxos(&self) -> Result<Vec<Utxo>> {
        let (v1, v2) = tokio::try_join!(
            self.kupo.get(POOL_ADDRESS_V1, true),
            self.kupo.get(POOL_ADDRESS_V2, true),
        )?;
        let mut all = v1;
        all.extend(v2);
        Ok(all)
    }

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

    /// Fetch datum, update fee and apply LovelaceDeduction.
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
        let d = parse_pool_datum(&datum)?;

        pool.total_lp_tokens = d.total_lp;
        pool.pool_fee_percent = d.final_fee as f64 / 100.0;

        // Apply lovelace deduction to whichever side holds ADA
        if d.lovelace_deduction != 0 {
            let deduction = d.lovelace_deduction.unsigned_abs();
            if pool.asset_a.is_lovelace() {
                pool.reserve_a = pool.reserve_a.saturating_sub(deduction);
            } else if pool.asset_b.is_lovelace() {
                pool.reserve_b = pool.reserve_b.saturating_sub(deduction);
            }
        }

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
                    Err(e) => eprintln!("[sundaeswap_v3] datum error {}: {}", utxo.tx_hash, e),
                }
            }
        }

        Ok(pools)
    }
}
