//! Fluent builder for a Minswap V2 swap or limit order.

use anyhow::{anyhow, Result};

use crate::address::{decode_base_address, WalletAddress};
use crate::dex::DexSwap;
use crate::models::{LiquidityPool, Token, Utxo};
use crate::requests::types::{OrderKind, PayToAddress, SwapFee, SwapParams};

#[derive(Debug, Clone, Copy, PartialEq)]
enum PricingMode {
    Slippage(f64),       // Market: derive min_receive from current price + slippage %
    MinReceive(u64),     // Limit:  raw value provided by the caller
    Unset,
}

pub struct SwapRequest<'a, D: DexSwap + ?Sized> {
    dex: &'a D,
    pool: Option<LiquidityPool>,
    swap_in_token: Option<Token>,
    swap_in_amount: u64,
    pricing: PricingMode,
    kill_on_failed: bool,
    sender: Option<WalletAddress>,
    receiver: Option<WalletAddress>,
    spend_utxos: Vec<Utxo>,
}

impl<'a, D: DexSwap + ?Sized> SwapRequest<'a, D> {
    pub fn new(dex: &'a D) -> Self {
        Self {
            dex,
            pool: None,
            swap_in_token: None,
            swap_in_amount: 0,
            pricing: PricingMode::Unset,
            kill_on_failed: false,
            sender: None,
            receiver: None,
            spend_utxos: vec![],
        }
    }

    pub fn for_pool(mut self, pool: LiquidityPool) -> Self {
        self.pool = Some(pool); self
    }

    pub fn with_swap_in_token(mut self, token: Token) -> Self {
        self.swap_in_token = Some(token); self
    }

    pub fn with_swap_in_amount(mut self, amount: u64) -> Self {
        self.swap_in_amount = amount; self
    }

    pub fn with_slippage_percent(mut self, slippage: f64) -> Self {
        self.pricing = PricingMode::Slippage(slippage); self
    }

    pub fn with_minimum_receive(mut self, raw: u64) -> Self {
        self.pricing = PricingMode::MinReceive(raw); self
    }

    pub fn with_kill_on_failed(mut self, kill: bool) -> Self {
        self.kill_on_failed = kill; self
    }

    pub fn with_sender_address(mut self, bech32: &str) -> Result<Self> {
        let w = decode_base_address(bech32)?;
        self.receiver.get_or_insert(w.clone());
        self.sender = Some(w);
        Ok(self)
    }

    pub fn with_receiver_address(mut self, bech32: &str) -> Result<Self> {
        self.receiver = Some(decode_base_address(bech32)?);
        Ok(self)
    }

    pub fn with_spend_utxos(mut self, utxos: Vec<Utxo>) -> Self {
        self.spend_utxos = utxos; self
    }

    pub fn get_swap_fees(&self) -> Vec<SwapFee> { self.dex.swap_order_fees() }

    pub fn get_estimated_receive(&self) -> Result<u64> {
        let pool = self.pool.as_ref().ok_or_else(|| anyhow!("pool not set"))?;
        let in_token = self.swap_in_token.as_ref().ok_or_else(|| anyhow!("swap_in_token not set"))?;
        Ok(self.dex.estimated_receive(pool, in_token, self.swap_in_amount))
    }

    pub fn get_minimum_receive(&self) -> Result<u64> {
        match self.pricing {
            PricingMode::MinReceive(v) => Ok(v),
            PricingMode::Slippage(s) => {
                let est = self.get_estimated_receive()? as f64;
                Ok((est / (1.0 + s / 100.0)).floor() as u64)
            }
            PricingMode::Unset => Err(anyhow!(
                "must call with_slippage_percent (market) or with_minimum_receive (limit)"
            )),
        }
    }

    pub fn get_price_impact_percent(&self) -> Result<f64> {
        let pool = self.pool.as_ref().ok_or_else(|| anyhow!("pool not set"))?;
        let in_token = self.swap_in_token.as_ref().ok_or_else(|| anyhow!("swap_in_token not set"))?;
        Ok(self.dex.price_impact_percent(pool, in_token, self.swap_in_amount))
    }

    /// Build the payment instructions.
    pub fn build(self) -> Result<Vec<PayToAddress>> {
        let pool = self.pool.clone().ok_or_else(|| anyhow!("pool not set"))?;
        let in_token = self.swap_in_token.clone().ok_or_else(|| anyhow!("swap_in_token not set"))?;
        if self.swap_in_amount == 0 {
            return Err(anyhow!("swap_in_amount must be > 0"));
        }
        let sender = self.sender.clone().ok_or_else(|| anyhow!("sender address not set"))?;
        let receiver = self.receiver.clone().unwrap_or_else(|| sender.clone());

        // Choose out token from pool (the one that's not in_token).
        let out_token = {
            use crate::models::token_identifier;
            let in_id = token_identifier(&in_token);
            if token_identifier(&pool.asset_a) == in_id {
                pool.asset_b.clone()
            } else if token_identifier(&pool.asset_b) == in_id {
                pool.asset_a.clone()
            } else {
                return Err(anyhow!("swap_in_token is not in this pool"));
            }
        };

        let kind = match self.pricing {
            PricingMode::Slippage(_) => OrderKind::Market,
            PricingMode::MinReceive(_) => OrderKind::Limit,
            PricingMode::Unset => {
                return Err(anyhow!(
                    "must call with_slippage_percent (market) or with_minimum_receive (limit)"
                ));
            }
        };
        let min_receive = self.get_minimum_receive()?;

        let params = SwapParams {
            sender,
            receiver,
            swap_in_token: in_token,
            swap_out_token: out_token,
            swap_in_amount: self.swap_in_amount,
            min_receive,
            kind,
            kill_on_failed: self.kill_on_failed,
            spend_utxos: self.spend_utxos,
        };
        self.dex.build_swap_order(&pool, &params)
    }
}
