use crate::models::Token;
use serde::Serialize;

/// A single open order on the ChadSwap order book.
#[derive(Debug, Clone, Serialize)]
pub struct Order {
    /// Token being traded (asset field from datum)
    pub asset: Token,
    /// Remaining unfilled amount (RemainingAmount from datum)
    pub amount: u64,
    /// Unit price numerator (UnitPrice from datum)
    pub price: u64,
    /// Unit price denominator — defaults to 1 when datum encodes null
    pub price_denominator: u64,
    /// true = buy order (ADA-only UTXO), false = sell order (ADA + token UTXO)
    pub is_buy: bool,
}

/// All open buy and sell orders for a specific token on ChadSwap.
#[derive(Debug, Clone, Serialize)]
pub struct OrderBook {
    pub token_id: String,
    pub buy_orders: Vec<Order>,
    pub sell_orders: Vec<Order>,
}
