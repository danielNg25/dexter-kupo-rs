use crate::models::{token_name, Token};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquidityPool {
    pub dex_identifier: String,
    pub asset_a: Token,
    pub asset_b: Token,
    pub reserve_a: u64,
    pub reserve_b: u64,
    pub address: String,
    pub pool_id: String,
    pub pool_fee_percent: f64,
    pub total_lp_tokens: u64,
}

impl LiquidityPool {
    pub fn new(
        dex_identifier: &str,
        asset_a: Token,
        asset_b: Token,
        reserve_a: u64,
        reserve_b: u64,
        address: &str,
        pool_fee_percent: f64,
        pool_id: &str,
    ) -> Self {
        Self {
            dex_identifier: dex_identifier.to_string(),
            asset_a,
            asset_b,
            reserve_a,
            reserve_b,
            address: address.to_string(),
            pool_id: pool_id.to_string(),
            pool_fee_percent,
            total_lp_tokens: 0,
        }
    }

    pub fn pair(&self) -> String {
        let asset_a_name = token_name(&self.asset_a);
        let asset_b_name = token_name(&self.asset_b);
        format!("{}/{}", asset_a_name, asset_b_name)
    }

    pub fn price(&self) -> f64 {
        let asset_a_decimals: i32 = match &self.asset_a {
            Token::Lovelace => 6,
            Token::Asset(a) => a.decimals as i32,
        };
        let asset_b_decimals: i32 = match &self.asset_b {
            Token::Lovelace => 6,
            Token::Asset(a) => a.decimals as i32,
        };

        let adjusted_reserve_a = self.reserve_a as f64 / 10_f64.powi(asset_a_decimals);
        let adjusted_reserve_b = self.reserve_b as f64 / 10_f64.powi(asset_b_decimals);

        if adjusted_reserve_b == 0.0 {
            return 0.0;
        }

        adjusted_reserve_a / adjusted_reserve_b
    }

    pub fn uuid(&self) -> String {
        format!("{}.{}.{}", self.dex_identifier, self.pair(), self.pool_id)
    }
}
