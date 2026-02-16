/// ChadSwap — order book DEX implementation.
///
/// ChadSwap is NOT an AMM liquidity pool DEX. It maintains open buy/sell orders
/// at two contract addresses. Each order UTXO encodes a price, token, and
/// remaining unfilled amount in its datum.
///
/// Unlike pool-based DEXes, this module does NOT implement `BaseDex`.
/// Use `get_orders_by_token(token_id)` to query the order book for a token.
///
/// Buy vs sell detection:
///   - Buy order:  UTXO holds only ADA (amount.len() == 1)
///   - Sell order: UTXO holds ADA + token (amount.len() > 1)
///
/// Datum structure (all constructors are Tag(121+alt, [...])):
///   Constr(0, [
///     Constr(0, [                      ← order info
///       Constr(0, [...]),              ← address fields (skip)
///       Constr(0|1, []),              ← direction (skip)
///       bytes,                         ← TokenPolicyId
///       bytes,                         ← TokenAssetName
///       int,                           ← UnitPrice
///       Constr(0, [int]) | Constr(1,[])← UnitPriceDenominator (null → 1)
///       Constr(1, []),                 ← (skip)
///       Constr(1, []),                 ← (skip)
///     ]),
///     Constr(0, [int, int]),           ← order state [RemainingAmount, FilledAmount]
///     ...                              ← (ignored)
///   ])
use anyhow::{anyhow, Result};
use ciborium::value::Value;
use tokio::try_join;

use crate::kupo::KupoApi;
use crate::models::asset::from_identifier;
use crate::models::{token_identifier, Order, OrderBook, Utxo};
use super::cbor::{constr_fields, decode_cbor, value_to_hex, value_to_u64};

const IDENTIFIER: &str = "ChadSwap";
const ORDER_ADDRESS_1: &str =
    "addr1wxxxdudv3dtaa09tngrm8wds54v45kkhdcau4e6keqh0uncksc7pn";
const ORDER_ADDRESS_2: &str =
    "addr1w84q0y2wwfj5efd9ch3x492edeh6pdwycvt7g030jfzhagg5ftr54";

pub struct ChadSwap {
    kupo: KupoApi,
}

impl ChadSwap {
    pub fn new(kupo: KupoApi) -> Self {
        Self { kupo }
    }

    pub fn identifier(&self) -> &str {
        IDENTIFIER
    }

    /// Fetch all open order UTXOs from both contract addresses.
    pub async fn all_order_utxos(&self) -> Result<Vec<Utxo>> {
        let (mut a, b) = try_join!(
            self.kupo.get(ORDER_ADDRESS_1, true),
            self.kupo.get(ORDER_ADDRESS_2, true),
        )?;
        a.extend(b);
        Ok(a)
    }

    /// Fetch the order book for a specific token identifier.
    ///
    /// `token_id` — the concatenated policy+name hex (no dot separator),
    ///              e.g. `"7507734918533b3b896241b4704f3d4ce805256b01da6fcede43043642616279534e454b"`
    pub async fn get_orders_by_token(&self, token_id: &str) -> Result<OrderBook> {
        let utxos = self.all_order_utxos().await?;

        let mut buy_orders = Vec::new();
        let mut sell_orders = Vec::new();

        for utxo in &utxos {
            let order = match self.order_from_utxo(utxo).await {
                Ok(Some(o)) => o,
                Ok(None) => continue,
                Err(e) => {
                    eprintln!("[chadswap] skipping {}: {}", utxo.tx_hash, e);
                    continue;
                }
            };

            let asset_id = token_identifier(&order.asset);
            if asset_id != token_id {
                continue;
            }

            if order.is_buy {
                buy_orders.push(order);
            } else {
                sell_orders.push(order);
            }
        }

        Ok(OrderBook {
            token_id: token_id.to_string(),
            buy_orders,
            sell_orders,
        })
    }

    /// Parse a single UTXO into an Order (fetches datum from Kupo).
    /// Returns None if the UTXO has no datum or the datum cannot be parsed.
    async fn order_from_utxo(&self, utxo: &Utxo) -> Result<Option<Order>> {
        let data_hash = match &utxo.data_hash {
            Some(h) => h.clone(),
            None => return Ok(None),
        };

        let cbor_hex = self.kupo.datum(&data_hash).await?;
        let datum = match parse_order_datum(&cbor_hex) {
            Ok(d) => d,
            Err(e) => {
                return Err(anyhow!("datum parse error for {}: {}", utxo.tx_hash, e));
            }
        };

        // Buy order: only ADA in UTXO (amount.len() == 1 after Kupo normalises)
        let is_buy = utxo.amount.len() == 1;

        let asset_id = datum.token_policy_id + &datum.token_asset_name;
        let asset = from_identifier(&asset_id, 0);

        Ok(Some(Order {
            asset,
            amount: datum.remaining_amount,
            price: datum.unit_price,
            price_denominator: datum.unit_price_denominator.unwrap_or(1),
            is_buy,
        }))
    }
}

// ── Datum parsing ─────────────────────────────────────────────────────────────

struct OrderDatum {
    token_policy_id: String,
    token_asset_name: String,
    unit_price: u64,
    /// None when encoded as Constr(1, []) — treated as 1 by the caller
    unit_price_denominator: Option<u64>,
    remaining_amount: u64,
}

/// Parse a ChadSwap order datum from CBOR hex.
///
/// Expected structure (Tag values: 121 = Constr(0), 122 = Constr(1)):
///   Tag(121)[                              ← outer Constr(0)
///     Tag(121)[                            ← order info Constr(0)
///       Tag(121)[Tag(121)[bytes], ...],    ← address info (skip — index 0)
///       Tag(121|122)[],                   ← direction (skip — index 1)
///       bytes,                             ← TokenPolicyId (index 2)
///       bytes,                             ← TokenAssetName (index 3)
///       int,                               ← UnitPrice (index 4)
///       Tag(121)[int] | Tag(122)[],        ← UnitPriceDenominator (index 5)
///       Tag(122)[],                        ← (skip — index 6)
///       Tag(122)[],                        ← (skip — index 7)
///     ],
///     Tag(121)[int, int],                  ← order state (index 1)
///     ...                                  ← ignored tail
///   ]
fn parse_order_datum(cbor_hex: &str) -> Result<OrderDatum> {
    let value = decode_cbor(cbor_hex)?;

    // Outer Constr(0, [...])
    let outer = constr_fields(&value)?;
    if outer.len() < 2 {
        return Err(anyhow!(
            "ChadSwap datum: outer expected >=2 fields, got {}",
            outer.len()
        ));
    }

    // outer[0] = order info Constr(0, [...])
    let info = constr_fields(&outer[0])?;
    if info.len() < 6 {
        return Err(anyhow!(
            "ChadSwap datum: order info expected >=6 fields, got {}",
            info.len()
        ));
    }
    // info[0] = address info (nested constructors) — skip
    // info[1] = direction Constr(0|1, []) — skip
    // info[2] = bytes: TokenPolicyId
    let token_policy_id = value_to_hex(&info[2])
        .map_err(|e| anyhow!("ChadSwap datum TokenPolicyId: {}", e))?;
    // info[3] = bytes: TokenAssetName
    let token_asset_name = value_to_hex(&info[3])
        .map_err(|e| anyhow!("ChadSwap datum TokenAssetName: {}", e))?;
    // info[4] = int: UnitPrice
    let unit_price = value_to_u64(&info[4])
        .map_err(|e| anyhow!("ChadSwap datum UnitPrice: {}", e))?;
    // info[5] = Constr(0, [int]) or Constr(1, []) for null
    let unit_price_denominator = parse_price_denominator(&info[5])?;

    // outer[1] = order state Constr(0, [remaining, filled])
    let state = constr_fields(&outer[1])?;
    if state.is_empty() {
        return Err(anyhow!("ChadSwap datum: order state is empty"));
    }
    let remaining_amount = value_to_u64(&state[0])
        .map_err(|e| anyhow!("ChadSwap datum RemainingAmount: {}", e))?;

    Ok(OrderDatum {
        token_policy_id,
        token_asset_name,
        unit_price,
        unit_price_denominator,
        remaining_amount,
    })
}

/// Parse the UnitPriceDenominator field.
///
/// Two variants:
///   - Constr(0, [int]) — denominator is the integer value
///   - Constr(1, [])   — null variant, returns None (caller substitutes 1)
fn parse_price_denominator(v: &Value) -> Result<Option<u64>> {
    let fields = constr_fields(v)?;
    if fields.is_empty() {
        // Constr(1, []) or Constr(0, []) — null variant
        return Ok(None);
    }
    let denom = value_to_u64(&fields[0])
        .map_err(|e| anyhow!("ChadSwap datum UnitPriceDenominator: {}", e))?;
    Ok(Some(denom))
}
