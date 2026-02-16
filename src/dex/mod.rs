use async_trait::async_trait;
use crate::models::{Utxo, LiquidityPool};
use crate::kupo::KupoApi;

pub mod cbor;
pub mod minswap_v1;
pub mod minswap_v2;
pub mod minswap_stable;
pub mod sundaeswap_v1;
pub mod sundaeswap_v3;
pub mod wingriders;
pub mod wingriders_v2;
pub mod cswap;
pub mod vyfinance;
pub mod vyfi_bar;

#[async_trait]
pub trait BaseDex: Send + Sync {
    fn identifier(&self) -> &str;
    fn pool_address(&self) -> &str;
    fn lp_token_policy_id(&self) -> &str;
    
    fn kupo(&self) -> &KupoApi;
    
    async fn all_liquidity_pool_utxos(&self) -> Result<Vec<Utxo>, anyhow::Error>;
    
    async fn liquidity_pool_from_utxo(
        &self,
        utxo: &Utxo,
        pool_id: &str
    ) -> Result<Option<LiquidityPool>, anyhow::Error>;

    /// Like liquidity_pool_from_utxo but also fetches the datum for accurate
    /// reserves and fee. Default implementation just calls liquidity_pool_from_utxo
    /// (suitable for DEXes that don't need datum parsing).
    async fn liquidity_pool_from_utxo_extend(
        &self,
        utxo: &Utxo,
        pool_id: &str
    ) -> Result<Option<LiquidityPool>, anyhow::Error> {
        self.liquidity_pool_from_utxo(utxo, pool_id).await
    }

    async fn liquidity_pool_from_pool_id(
        &self, 
        pool_id: &str
    ) -> Result<Option<LiquidityPool>, anyhow::Error>;
    
    async fn liquidity_pools_from_token(
        &self, 
        token_b: &str, 
        token_a: &str
    ) -> Result<Vec<LiquidityPool>, anyhow::Error>;
}
