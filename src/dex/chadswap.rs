/// ChadSwap — order book DEX implementation.
///
/// ChadSwap is NOT an AMM liquidity pool DEX. It maintains open buy/sell orders
/// fetched directly from the ChadSwap API (`https://api.chadswap.com/orders`).
///
/// Unlike pool-based DEXes, this module does NOT implement `BaseDex`.
/// Use `get_orders_by_token(token_id)` or `get_all_order_books()` to query orders.
use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::collections::HashMap;

use crate::models::asset::{from_identifier, token_identifier};
use crate::models::{Order, OrderBook};

const IDENTIFIER: &str = "ChadSwap";
const CHADSWAP_API_URL: &str = "https://api.chadswap.com/orders";

pub struct ChadSwap {
    client: reqwest::Client,
}

impl ChadSwap {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent("Mozilla/5.0 (compatible; dexter-kupo-rs/0.1)")
            .build()
            .expect("Failed to build HTTP client");
        Self { client }
    }

    pub fn identifier(&self) -> &str {
        IDENTIFIER
    }

    /// Fetch the order book for a specific token identifier.
    ///
    /// `token_id` — the concatenated policy+name hex (no dot separator),
    ///              e.g. `"7507734918533b3b896241b4704f3d4ce805256b01da6fcede43043642616279534e454b"`
    pub async fn get_orders_by_token(&self, token_id: &str) -> Result<OrderBook> {
        let api_orders = self.fetch_api_orders().await?;

        let mut buy_orders = Vec::new();
        let mut sell_orders = Vec::new();
        let mut skipped = 0u32;

        for api_order in &api_orders {
            // API may return unit as "policy_id.asset_name" or "policy_id" + "asset_name"
            let unit_normalized = api_order.unit.replace('.', "");
            if unit_normalized != token_id {
                continue;
            }
            match order_from_api_order(api_order) {
                Ok(order) => {
                    if order.is_buy {
                        buy_orders.push(order);
                    } else {
                        sell_orders.push(order);
                    }
                }
                Err(e) => {
                    skipped += 1;
                    eprintln!("[chadswap] skipping {}: {}", api_order.output_ref, e);
                }
            }
        }

        if skipped > 0 {
            eprintln!("[chadswap] get_orders_by_token: skipped {} malformed orders", skipped);
        }

        Ok(OrderBook {
            token_id: token_id.to_string(),
            buy_orders,
            sell_orders,
        })
    }

    /// Fetch all orders across all tokens, grouped by token identifier.
    pub async fn get_all_order_books(&self) -> Result<HashMap<String, OrderBook>> {
        let api_orders = self.fetch_api_orders().await?;

        let mut books: HashMap<String, OrderBook> = HashMap::new();
        let mut parsed_ok = 0u32;
        let mut skipped = 0u32;

        for api_order in &api_orders {
            match order_from_api_order(api_order) {
                Ok(order) => {
                    parsed_ok += 1;
                    let asset_id = token_identifier(&order.asset);
                    let book = books.entry(asset_id.clone()).or_insert_with(|| OrderBook {
                        token_id: asset_id,
                        buy_orders: Vec::new(),
                        sell_orders: Vec::new(),
                    });
                    if order.is_buy {
                        book.buy_orders.push(order);
                    } else {
                        book.sell_orders.push(order);
                    }
                }
                Err(e) => {
                    skipped += 1;
                    eprintln!("[chadswap] skipping {}: {}", api_order.output_ref, e);
                }
            }
        }

        eprintln!(
            "[chadswap] get_all_order_books: parsed={}, skipped={}, tokens={}",
            parsed_ok, skipped, books.len()
        );

        Ok(books)
    }

    async fn fetch_api_orders(&self) -> Result<Vec<ChadSwapApiOrder>> {
        let resp = self.client.get(CHADSWAP_API_URL).send().await?;
        if !resp.status().is_success() {
            return Err(anyhow!("ChadSwap API returned status {}", resp.status()));
        }
        let body: ChadSwapApiResponse = resp.json().await?;
        Ok(body.results)
    }
}

// ── ChadSwap API types ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ChadSwapApiResponse {
    results: Vec<ChadSwapApiOrder>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChadSwapApiOrder {
    output_ref: String,
    #[serde(rename = "type")]
    order_type: String,
    unit: String,
    unit_price: String,
    // null when price denominator is 1 (implicit)
    #[serde(default)]
    unit_price_denom: Option<String>,
    // null for fully-filled orders
    #[serde(default)]
    tokens_left: Option<String>,
}

// ── API → Order mapping ───────────────────────────────────────────────────────

fn order_from_api_order(api_order: &ChadSwapApiOrder) -> Result<Order> {
    let unit_normalized = api_order.unit.replace('.', "");
    let asset = from_identifier(&unit_normalized, 0);

    let amount: u64 = api_order
        .tokens_left
        .as_deref()
        .unwrap_or("0")
        .parse()
        .map_err(|e| anyhow!("tokens_left parse error for {}: {}", api_order.output_ref, e))?;

    let price: u64 = api_order
        .unit_price
        .parse()
        .map_err(|e| anyhow!("unit_price parse error for {}: {}", api_order.output_ref, e))?;

    let price_denominator: u64 = api_order
        .unit_price_denom
        .as_deref()
        .unwrap_or("1")
        .parse()
        .map_err(|e| anyhow!("unit_price_denom parse error for {}: {}", api_order.output_ref, e))?;

    let is_buy = api_order.order_type == "buy";

    Ok(Order {
        asset,
        amount,
        price,
        price_denominator,
        is_buy,
    })
}
