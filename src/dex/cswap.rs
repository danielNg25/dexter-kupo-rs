use anyhow::{anyhow, Result};
use async_trait::async_trait;
use crate::kupo::KupoApi;
use crate::models::asset::from_identifier;
use crate::models::{token_identifier, LiquidityPool, Utxo};
use super::BaseDex;
use super::cbor::{constr_fields, decode_cbor, value_to_u64};

const IDENTIFIER: &str = "CSWAP";
const POOL_ADDRESS: &str =
    "addr1z8ke0c9p89rjfwmuh98jpt8ky74uy5mffjft3zlcld9h7ml3lmln3mwk0y3zsh3gs3dzqlwa9rjzrxawkwm4udw9axhs6fuu6e";
/// LP tokens are identified by their asset name hex being exactly "63"
const LP_TOKEN_NAME_HEX: &str = "63";

pub struct CSwap {
    kupo: KupoApi,
}

impl CSwap {
    pub fn new(kupo: KupoApi) -> Self {
        Self { kupo }
    }
}

/// Parse CSwap pool datum.
///
/// Structure (constructor 0):
///   [0]: int   — TotalLpTokens
///   [1]: int   — LpFee  → fee = LpFee / 100
///   [2]: bytes — PoolAssetAPolicyId (ignored)
///   [3]: bytes — PoolAssetAAssetName (ignored)
///   [4]: bytes — PoolAssetBPolicyId (ignored)
///   [5]: bytes — PoolAssetBAssetName (ignored)
///   [6]: (ignored)
struct CSwapDatum {
    total_lp: u64,
    lp_fee: u64,
}

fn parse_pool_datum(cbor_hex: &str) -> Result<CSwapDatum> {
    let value = decode_cbor(cbor_hex)?;
    let fields = constr_fields(&value)?;

    if fields.len() < 2 {
        return Err(anyhow!(
            "CSwap datum: expected >=2 fields, got {}",
            fields.len()
        ));
    }

    let total_lp = value_to_u64(&fields[0])?;
    let lp_fee = value_to_u64(&fields[1])?;

    Ok(CSwapDatum { total_lp, lp_fee })
}

/// Return true if this asset unit is a CSwap LP token.
/// JS: `assetBalanceId.slice(56) == "63"`
fn is_lp_token(unit: &str) -> bool {
    unit.len() > 56 && &unit[56..] == LP_TOKEN_NAME_HEX
}

#[async_trait]
impl BaseDex for CSwap {
    fn identifier(&self) -> &str {
        IDENTIFIER
    }

    fn pool_address(&self) -> &str {
        POOL_ADDRESS
    }

    fn lp_token_policy_id(&self) -> &str {
        // CSwap LP tokens don't share a single policy — identified by name hex "63"
        LP_TOKEN_NAME_HEX
    }

    fn kupo(&self) -> &KupoApi {
        &self.kupo
    }

    async fn all_liquidity_pool_utxos(&self) -> Result<Vec<Utxo>> {
        // Kupo: bech32 addresses are queried directly (no /* wildcard)
        self.kupo.get(POOL_ADDRESS, true).await
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

        // Filter out LP tokens (name hex == "63"), capture as pool_id
        let relevant: Vec<_> = utxo
            .amount
            .iter()
            .filter(|a| {
                if is_lp_token(&a.unit) {
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

        let a_unit = &relevant[a_idx].unit;
        let b_unit = &relevant[b_idx].unit;

        let reserve_a = relevant[a_idx].quantity.parse::<u64>()?;
        let reserve_b = relevant[b_idx].quantity.parse::<u64>()?;

        let asset_a = from_identifier(a_unit, 0);
        let asset_b = from_identifier(b_unit, 0);

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

    /// Fetch datum and update fee from LpFee field.
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
        pool.pool_fee_percent = d.lp_fee as f64 / 100.0;

        Ok(Some(pool))
    }

    async fn liquidity_pool_from_pool_id(&self, pool_id: &str) -> Result<Option<LiquidityPool>> {
        let utxos = self.all_liquidity_pool_utxos().await?;
        let found = utxos
            .iter()
            .find(|u| u.amount.iter().any(|a| a.unit == pool_id));

        match found {
            Some(utxo) => self.liquidity_pool_from_utxo_extend(utxo, pool_id).await,
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
                    Err(e) => eprintln!("[cswap] datum error {}: {}", utxo.tx_hash, e),
                }
            }
        }

        Ok(pools)
    }
}
