use crate::models::{token_name, Token};
use serde::Serialize;

/// A Curve-style stable swap liquidity pool.
///
/// Unlike regular AMM pools, stable pools use an amplification coefficient (A)
/// and a D-invariant (total_liquidity) to maintain near-1:1 pricing between
/// pegged assets (e.g. USDC/iUSD, ADA/stADA).
///
/// Reserves are read from the datum (Balance0/Balance1), NOT from UTXO amounts.
#[derive(Debug, Clone, Serialize)]
pub struct StablePool {
    pub dex_identifier: String,
    pub asset_a: Token,
    pub asset_b: Token,
    /// Balance0 from datum — reserve of asset_a
    pub reserve_a: u64,
    /// Balance1 from datum — reserve of asset_b
    pub reserve_b: u64,
    pub address: String,
    pub pool_id: String,
    pub pool_fee_percent: f64,
    /// Amplification coefficient (A) — controls curve flatness near peg
    pub amplification_coefficient: u64,
    /// Total liquidity invariant (D) — total liquidity across both assets
    pub total_liquidity: u64,
}

impl StablePool {
    pub fn pair(&self) -> String {
        format!(
            "{}/{}",
            token_name(&self.asset_a),
            token_name(&self.asset_b)
        )
    }

    /// Stable swap price using the derivative of the StableSwap invariant:
    ///
    ///   P = (y/x) * (1 + A*x/D) / (1 + A*y/D)
    ///
    /// where x, y are decimal-adjusted balances, A is the amplification
    /// coefficient, and D is the total liquidity invariant.
    pub fn price(&self) -> f64 {
        let dec_a = match &self.asset_a {
            Token::Lovelace => 6,
            Token::Asset(a) => a.decimals,
        };
        let dec_b = match &self.asset_b {
            Token::Lovelace => 6,
            Token::Asset(a) => a.decimals,
        };

        let x = self.reserve_a as f64 / 10_f64.powi(dec_a as i32);
        let y = self.reserve_b as f64 / 10_f64.powi(dec_b as i32);
        let a = self.amplification_coefficient as f64;
        let d = self.total_liquidity as f64 / 10_f64.powi(dec_a.min(dec_b) as i32);

        if x <= 0.0 || y <= 0.0 || d <= 0.0 {
            return 0.0;
        }

        (y / x) * ((1.0 + a * x / d) / (1.0 + a * y / d))
    }
}
