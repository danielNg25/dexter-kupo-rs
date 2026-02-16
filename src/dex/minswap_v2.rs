use anyhow::{Result, anyhow};
use async_trait::async_trait;
use crate::models::{Token, Utxo, LiquidityPool, token_identifier};
use crate::models::asset::from_identifier;
use crate::kupo::KupoApi;
use super::BaseDex;
use super::cbor::{constr_fields, value_to_u64, parse_asset_constr, decode_cbor};

const LP_TOKEN_POLICY_ID: &str = "f5808c2c990d86da54bfc97d89cee6efa20cd8461616359478d96b4c";
const POOL_VALIDITY_ASSET: &str = "f5808c2c990d86da54bfc97d89cee6efa20cd8461616359478d96b4c4d5350";
const POOL_SCRIPT_HASH_BECH32: &str = "script1agrmwv7exgffcdu27cn5xmnuhsh0p0ukuqpkhdgm800xksw7e2w";
const IDENTIFIER: &str = "MINSWAPV2";

pub struct MinswapV2 {
    kupo: KupoApi,
}

impl MinswapV2 {
    pub fn new(kupo: KupoApi) -> Self {
        Self { kupo }
    }

    fn compare_token_with_policy(token: &Token, policy: &str) -> bool {
        match token {
            Token::Lovelace => policy.is_empty() || policy == "lovelace",
            Token::Asset(a) => a.policy_id == policy || a.identifier("") == policy,
        }
    }

    pub async fn liquidity_pool_from_utxo_extend(
        &self,
        utxo: &Utxo,
        pool_id: &str,
    ) -> Result<Option<LiquidityPool>> {
        let mut liquidity_pool = match self.liquidity_pool_from_utxo(utxo, pool_id).await? {
            Some(pool) => pool,
            None => return Ok(None),
        };

        let data_hash = match &utxo.data_hash {
            Some(h) => h,
            None => return Ok(None),
        };

        let datum = self.kupo.datum(data_hash).await?;
        let parsed = parse_pool_datum(&datum)?;

        // Skip Zap pools (asset B policy == LP token policy), same as JS
        if parsed.pool_asset_b_policy == LP_TOKEN_POLICY_ID {
            return Ok(None);
        }

        liquidity_pool.pool_fee_percent = parsed.base_fee as f64 / 100.0;
        liquidity_pool.total_lp_tokens = parsed.total_lp_tokens;

        // Determine if the pool's asset_a order matches the datum's asset_a order.
        // In the datum, lovelace is represented as empty policy + empty name ("" + "").
        let datum_asset_a_is_lovelace = parsed.pool_asset_a_policy.is_empty()
            && parsed.pool_asset_a_name.is_empty();

        let pool_asset_a_matches = if datum_asset_a_is_lovelace {
            liquidity_pool.asset_a.is_lovelace()
        } else {
            let asset_a_identifier = token_identifier(&liquidity_pool.asset_a);
            asset_a_identifier == format!("{}{}", parsed.pool_asset_a_policy, parsed.pool_asset_a_name)
        };

        if pool_asset_a_matches {
            liquidity_pool.reserve_a = parsed.reserve_a;
            liquidity_pool.reserve_b = parsed.reserve_b;
        } else {
            liquidity_pool.reserve_a = parsed.reserve_b;
            liquidity_pool.reserve_b = parsed.reserve_a;
        }

        Ok(Some(liquidity_pool))
    }
}

#[derive(Debug)]
struct PoolDatum {
    reserve_a: u64,
    reserve_b: u64,
    pool_asset_a_policy: String,
    pool_asset_a_name: String,
    pool_asset_b_policy: String,
    pool_asset_b_name: String,
    total_lp_tokens: u64,
    base_fee: u64,
}

/// Parse the MinswapV2 pool datum CBOR hex.
///
/// Datum structure (constructor 0):
///   [0] validator_hash  (constr, ignored)
///   [1] asset_a         (constr {policy_bytes, name_bytes})
///   [2] asset_b         (constr {policy_bytes, name_bytes})
///   [3] total_lp_tokens (int)
///   [4] reserve_a       (int)
///   [5] reserve_b       (int)
///   [6] base_fee        (int)   -- divide by 100 to get percent
///   [7] fee_sharing_numerator (int)
///   [8] ...
fn parse_pool_datum(cbor_hex: &str) -> Result<PoolDatum> {
    let value = decode_cbor(cbor_hex)?;

    // Top-level constr (tag 121 = constructor 0)
    let fields = constr_fields(&value)?;

    if fields.len() < 7 {
        return Err(anyhow!("Expected at least 7 fields in pool datum, got {}", fields.len()));
    }

    // fields[0]: validator wrapper — skip
    // fields[1]: asset A constr
    let (pool_asset_a_policy, pool_asset_a_name) = parse_asset_constr(&fields[1])?;
    // fields[2]: asset B constr
    let (pool_asset_b_policy, pool_asset_b_name) = parse_asset_constr(&fields[2])?;
    // fields[3]: total LP tokens
    let total_lp_tokens = value_to_u64(&fields[3])?;
    // fields[4]: reserve A
    let reserve_a = value_to_u64(&fields[4])?;
    // fields[5]: reserve B
    let reserve_b = value_to_u64(&fields[5])?;
    // fields[6]: base fee (e.g. 100 → 1.0%, 30 → 0.3%)
    let base_fee = value_to_u64(&fields[6])?;

    Ok(PoolDatum {
        reserve_a,
        reserve_b,
        pool_asset_a_policy,
        pool_asset_a_name,
        pool_asset_b_policy,
        pool_asset_b_name,
        total_lp_tokens,
        base_fee,
    })
}

#[async_trait]
impl BaseDex for MinswapV2 {
    fn identifier(&self) -> &str {
        IDENTIFIER
    }

    fn pool_address(&self) -> &str {
        POOL_SCRIPT_HASH_BECH32
    }

    fn lp_token_policy_id(&self) -> &str {
        LP_TOKEN_POLICY_ID
    }

    fn kupo(&self) -> &KupoApi {
        &self.kupo
    }

    async fn all_liquidity_pool_utxos(&self) -> Result<Vec<Utxo>> {
        let pattern = format!("{}/{}", POOL_SCRIPT_HASH_BECH32, "*");
        self.kupo.get(&pattern, true).await
    }

    async fn liquidity_pool_from_utxo(&self, utxo: &Utxo, pool_id: &str) -> Result<Option<LiquidityPool>> {
        // Filter out UTXOs without data_hash (same as JS)
        if utxo.data_hash.is_none() {
            return Ok(None);
        }

        let relevant_assets: Vec<_> = utxo.amount.iter()
            .filter(|asset| {
                let unit = &asset.unit;
                unit != POOL_VALIDITY_ASSET && !unit.starts_with(LP_TOKEN_POLICY_ID)
            })
            .collect();

        if relevant_assets.len() < 2 {
            return Ok(None);
        }

        let asset_a_idx = if relevant_assets.len() == 2 { 0 } else { 1 };
        let asset_b_idx = if relevant_assets.len() == 2 { 1 } else { 2 };

        // Find pool ID - get the FIRST one that matches (same as JS .find())
        let pool_id = utxo.amount.iter()
            .find(|a| {
                let unit = &a.unit;
                unit.starts_with(LP_TOKEN_POLICY_ID) && *unit != POOL_VALIDITY_ASSET
            })
            .map(|a| a.unit.clone())
            .unwrap_or_else(|| pool_id.to_string());

        let asset_a = from_identifier(&relevant_assets[asset_a_idx].unit, 0);
        let asset_b = from_identifier(&relevant_assets[asset_b_idx].unit, 0);

        let reserve_a = relevant_assets[asset_a_idx].quantity.parse::<u64>()?;
        let reserve_b = relevant_assets[asset_b_idx].quantity.parse::<u64>()?;

        let pool = LiquidityPool::new(
            IDENTIFIER,
            asset_a,
            asset_b,
            reserve_a,
            reserve_b,
            &utxo.address,
            0.3,
            &pool_id,
        );

        Ok(Some(pool))
    }

    async fn liquidity_pool_from_utxo_extend(&self, utxo: &Utxo, pool_id: &str) -> Result<Option<LiquidityPool>> {
        MinswapV2::liquidity_pool_from_utxo_extend(self, utxo, pool_id).await
    }

    async fn liquidity_pool_from_pool_id(&self, pool_id: &str) -> Result<Option<LiquidityPool>> {
        let full_pool_id = if pool_id.starts_with(LP_TOKEN_POLICY_ID) {
            pool_id.to_string()
        } else {
            format!("{}{}", LP_TOKEN_POLICY_ID, pool_id)
        };

        let utxos = self.all_liquidity_pool_utxos().await?;

        let found_utxo = utxos.iter()
            .find(|utxo| utxo.amount.iter().any(|a| a.unit == full_pool_id));

        match found_utxo {
            Some(utxo) => self.liquidity_pool_from_utxo_extend(utxo, &full_pool_id).await,
            None => Ok(None),
        }
    }

    async fn liquidity_pools_from_token(&self, token_b: &str, token_a: &str) -> Result<Vec<LiquidityPool>> {
        // Fetch all UTXOs at the pool address (single HTTP request),
        // filter by token identifiers from UTXO amounts (no datum needed),
        // then only fetch datums for the matching pools.
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
                match self.liquidity_pool_from_utxo_extend(utxo, &base.pool_id).await {
                    Ok(Some(extended)) => pools.push(extended),
                    Ok(None) => {}
                    Err(e) => eprintln!("[error] datum fetch for {}: {}", utxo.tx_hash, e),
                }
            }
        }

        Ok(pools)
    }
}
