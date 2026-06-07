//! `impl DexSwap for MinswapV2` — Minswap V2 order construction.
//!
//! Kept in a separate file from the reader (`minswap_v2.rs`) so each file has
//! one clear responsibility.

use anyhow::{anyhow, Result};

use crate::address::{decode_base_address, script_and_stake_to_base_address, WalletAddress};
use crate::dex::minswap_v2::MinswapV2;
use crate::dex::DexSwap;
use crate::models::{token_identifier, LiquidityPool, Token, Utxo};
use crate::plutus::PlutusData;
use crate::requests::{AddressType, AssetAmount, PayToAddress, PlutusScript, PlutusVersion, SpendUtxo, SwapFee, SwapParams, UtxoRef};

/// Returns `(reserve_for_token, reserve_for_other)` ordered to match `token`.
fn corresponding_reserves(pool: &LiquidityPool, token: &Token) -> (u128, u128) {
    let token_id = token_identifier(token);
    let a_id = token_identifier(&pool.asset_a);
    if token_id == a_id {
        (pool.reserve_a as u128, pool.reserve_b as u128)
    } else {
        (pool.reserve_b as u128, pool.reserve_a as u128)
    }
}

fn fee_mods(pool_fee_percent: f64) -> (u128, u128) {
    let mult: u128 = 10_000;
    // round-half-away-from-zero, mirroring dexter's Math.round
    let modifier = mult - (((pool_fee_percent / 100.0) * mult as f64).round() as u128);
    (mult, modifier)
}

/// Minswap V2 order script hash — payment credential of every V2 order address.
pub const ORDER_SCRIPT_HASH: &str = "c3e28c36c3447315ba5a56f33da6a6ddc1770a876a8d9f0cb3a97c4c";
/// The on-chain UTxO that holds the Minswap V2 order script as its
/// reference_script. Mainnet only. Discovered from tx
/// `ddb038fae5556b564b04e9dcf6139415d8be3b2e45a4e8c8eba29df79e47d169`.
pub const ORDER_SCRIPT_REF_UTXO_TX: &str =
    "cf4ecddde0d81f9ce8fcc881a85eb1f8ccdaf6807f03fea4cd02da896a621776";
/// Output index of [`ORDER_SCRIPT_REF_UTXO_TX`].
pub const ORDER_SCRIPT_REF_UTXO_INDEX: u64 = 0;
/// LP token policy id (also the pool validity-asset policy).
pub const LP_TOKEN_POLICY_ID: &str = "f5808c2c990d86da54bfc97d89cee6efa20cd8461616359478d96b4c";
/// Cancel redeemer — Constr(1,[]) (`unit`), per dexter's `cancelDatum`.
pub const CANCEL_REDEEMER: &str = "d87a80";

/// Order validator script CBOR — PlutusV2 — copied verbatim from dexter
/// `src/dex/minswap-v2.ts` `orderScript`. Used as the witnessed validator in
/// cancel transactions.
pub const ORDER_SCRIPT_CBOR_HEX: &str = "590a600100003332323232323232323222222533300832323232533300c3370e900118058008991919299980799b87480000084cc004dd5980a180a980a980a980a980a980a98068030060a99980799b87480080084c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c94ccc080cdc3a4000002264646600200200e44a66604c00229404c8c94ccc094cdc78010028a51133004004001302a002375c60500026eb8c094c07800854ccc080cdc3a40040022646464646600200202844a66605000229404c8c94ccc09ccdd798161812981618129816181698128010028a51133004004001302c002302a0013374a9001198131ba90014bd701bae3026001301e002153330203370e900200089980900419ba548000cc090cdd2a400466048604a603c00497ae04bd70099981019b87375a6044604a66446464a66604866e1d200200114bd6f7b63009bab302930220023022001323300100100322533302700114c103d87a800013232323253330283371e00e004266e9520003302c374c00297ae0133006006003375660520066eb8c09c008c0ac008c0a4004c8cc004004030894ccc09400452f5bded8c0264646464a66604c66e3d22100002100313302a337606ea4008dd3000998030030019bab3027003375c604a0046052004604e0026eb8c094c07800920004a0944c078004c08c004c06c060c8c8c8c8c8c8c94ccc08ccdc3a40000022646464646464646464646464646464646464a6660706076004264646464646464649319299981e99b87480000044c8c94ccc108c1140084c92632375a60840046eb4c10000458c8cdd81822000982218228009bac3043001303b0091533303d3370e90010008a999820181d8048a4c2c2c607601064a66607866e1d2000001132323232323232325333047304a002132498c09401458cdc3a400460886ea8c120004c120008dd6982300098230011822000982200119b8748008c0f8dd51821000981d0060a99981e19b87480080044c8c8c8c8c8c94ccc114c1200084c926302300316375a608c002608c0046088002608800466e1d2002303e3754608400260740182a66607866e1d2004001132323232323232325333047304a002132498c09401458dd6982400098240011bad30460013046002304400130440023370e9001181f1baa3042001303a00c1533303c3370e9003000899191919191919192999823982500109924c604a00a2c66e1d200230443754609000260900046eb4c118004c118008c110004c110008cdc3a4004607c6ea8c108004c0e803054ccc0f0cdc3a40100022646464646464a66608a60900042649319299982199b87480000044c8c8c8c94ccc128c13400852616375a609600260960046eb4c124004c10401854ccc10ccdc3a4004002264646464a666094609a0042930b1bad304b001304b002375a6092002608200c2c608200a2c66e1d200230423754608c002608c0046eb4c110004c110008c108004c0e803054ccc0f0cdc3a401400226464646464646464a66608e60940042649318130038b19b8748008c110dd5182400098240011bad30460013046002375a60880026088004608400260740182a66607866e1d200c001132323232323232325333047304a002132498c09801458cdc3a400460886ea8c120004c120008dd6982300098230011822000982200119b8748008c0f8dd51821000981d0060a99981e19b87480380044c8c8c8c8c8c8c8c8c8c8c8c8c8c94ccc134c14000852616375a609c002609c0046eb4c130004c130008dd6982500098250011bad30480013048002375a608c002608c0046eb4c110004c110008cdc3a4004607c6ea8c108004c0e803054ccc0f0cdc3a4020002264646464646464646464a66609260980042649318140048b19b8748008c118dd5182500098250011bad30480013048002375a608c002608c0046eb4c110004c110008c108004c0e803054ccc0f0cdc3a40240022646464646464a66608a60900042646493181200219198008008031129998238008a4c2646600600660960046464a66608c66e1d2000001132323232533304d3050002132498c0b400c58cdc3a400460946ea8c138004c138008c130004c11000858c110004c12400458dd698230009823001182200098220011bac3042001303a00c1533303c3370e900a0008a99981f981d0060a4c2c2c6074016603a018603001a603001c602c01e602c02064a66606c66e1d200000113232533303b303e002149858dd7181e000981a0090a99981b19b87480080044c8c94ccc0ecc0f800852616375c607800260680242a66606c66e1d200400113232533303b303e002149858dd7181e000981a0090a99981b19b87480180044c8c94ccc0ecc0f800852616375c607800260680242c60680222c607200260720046eb4c0dc004c0dc008c0d4004c0d4008c0cc004c0cc008c0c4004c0c4008c0bc004c0bc008c0b4004c0b4008c0ac004c0ac008c0a4004c08407858c0840748c94ccc08ccdc3a40000022a66604c60420042930b0a99981199b87480080044c8c94ccc0a0c0ac00852616375c605200260420042a66604666e1d2004001132325333028302b002149858dd7181480098108010b1810800919299981119b87480000044c8c8c8c94ccc0a4c0b00084c8c9263253330283370e9000000899192999816981800109924c64a66605666e1d20000011323253330303033002132498c04400458c0c4004c0a400854ccc0accdc3a40040022646464646464a666068606e0042930b1bad30350013035002375a606600260660046eb4c0c4004c0a400858c0a400458c0b8004c09800c54ccc0a0cdc3a40040022a666056604c0062930b0b181300118050018b18150009815001181400098100010b1810000919299981099b87480000044c8c94ccc098c0a400852616375a604e002603e0042a66604266e1d20020011323253330263029002149858dd69813800980f8010b180f800919299981019b87480000044c8c94ccc094c0a000852616375a604c002603c0042a66604066e1d20020011323253330253028002149858dd69813000980f0010b180f000919299980f99b87480000044c8c8c8c94ccc098c0a400852616375c604e002604e0046eb8c094004c07400858c0740048c94ccc078cdc3a400000226464a666046604c0042930b1bae3024001301c0021533301e3370e900100089919299981198130010a4c2c6eb8c090004c07000858c070004dd618100009810000980f8011bab301d001301d001301c00237566034002603400260320026030002602e0046eb0c054004c0340184cc004dd5980a180a980a980a980a980a980a980680300591191980080080191299980a8008a50132323253330153375e00c00229444cc014014008c054008c064008c05c004c03001cc94ccc034cdc3a40000022a666020601600e2930b0a99980699b874800800454ccc040c02c01c526161533300d3370e90020008a99980818058038a4c2c2c601600c2c60200026020004601c002600c00229309b2b118029baa001230033754002ae6955ceaab9e5573eae815d0aba24c126d8799fd87a9f581c1eae96baf29e27682ea3f815aba361a0c6059d45e4bfbe95bbd2f44affff004c0126d8799fd87a9f581cc8b0cc61374d409ff9c8512317003e7196a3e4d48553398c656cc124ffff0001";

pub const BATCHER_FEE_LOVELACE: u64 = 2_000_000;
pub const DEPOSIT_LOVELACE: u64 = 2_000_000;

/// Compute the V2 swap direction (`a_to_b_direction` in the Minswap spec, 0/1).
///   * Matches dexter: lexicographic compare of swap-in vs swap-out unit strings.
///   * "lovelace" is represented by the empty unit ("" + ""), which sorts first.
fn compute_direction(swap_in: &Token, swap_out: &Token) -> u64 {
    fn unit_for_sort(t: &Token) -> String {
        match t {
            Token::Lovelace => String::new(),
            Token::Asset(a) => format!("{}{}", a.policy_id, a.name_hex),
        }
    }
    let in_u = unit_for_sort(swap_in);
    let out_u = unit_for_sort(swap_out);
    if in_u <= out_u { 1 } else { 0 }
}

/// Match a UTxO whose address's payment credential is the V2 order script hash.
fn payment_credential_is_order_script(addr: &str) -> bool {
    matches!(decode_base_address(addr), Ok(w) if w.payment_key_hash == ORDER_SCRIPT_HASH)
}

/// Build the 9-field V2 OrderDatum.
fn build_v2_order_datum(
    sender_pkh_hex: &str,
    receiver: &WalletAddress,
    lp_policy_hex: &str,
    lp_name_hex: &str,
    direction: u64,
    swap_in_amount: u64,
    min_receive: u64,
    killable: bool,
    batcher_fee: u64,
) -> Result<PlutusData> {
    let stake_hex = receiver.staking_key_hash.as_deref().ok_or_else(|| {
        anyhow!("receiver address has no staking credential (required for V2 order)")
    })?;
    let r_pkh_hex = receiver.payment_key_hash.as_str();

    let canceller_pkh = PlutusData::bytes_hex(sender_pkh_hex)?;
    let r_pkh = PlutusData::bytes_hex(r_pkh_hex)?;
    let r_stake = PlutusData::bytes_hex(stake_hex)?;

    let wallet_address = PlutusData::Constr(0, vec![
        PlutusData::Constr(0, vec![r_pkh.clone()]),
        PlutusData::Constr(0, vec![
            PlutusData::Constr(0, vec![
                PlutusData::Constr(0, vec![r_stake.clone()]),
            ]),
        ]),
    ]);

    let lp_policy = PlutusData::bytes_hex(lp_policy_hex)?;
    let lp_name = PlutusData::bytes_hex(lp_name_hex)?;

    let killable_constr = if killable {
        PlutusData::Constr(1, vec![])
    } else {
        PlutusData::Constr(0, vec![])
    };

    Ok(PlutusData::Constr(0, vec![
        PlutusData::Constr(0, vec![canceller_pkh]),
        wallet_address.clone(),
        PlutusData::Constr(0, vec![]),
        wallet_address,
        PlutusData::Constr(0, vec![]),
        PlutusData::Constr(0, vec![lp_policy, lp_name]),
        PlutusData::Constr(0, vec![
            PlutusData::Constr(direction, vec![]),
            PlutusData::Constr(0, vec![PlutusData::Int(swap_in_amount as i128)]),
            PlutusData::Int(min_receive as i128),
            killable_constr,
        ]),
        PlutusData::Int(batcher_fee as i128),
        PlutusData::Constr(1, vec![]),
    ]))
}

impl DexSwap for MinswapV2 {
    fn identifier(&self) -> &str {
        "MinswapV2"
    }

    fn estimated_receive(&self, pool: &LiquidityPool, in_token: &Token, in_amount: u64) -> u64 {
        let (mult, modifier) = fee_mods(pool.pool_fee_percent);
        let (r_in, r_out) = corresponding_reserves(pool, in_token);
        let in_amt = in_amount as u128;
        let num = in_amt * r_out * modifier;
        let den = in_amt * modifier + r_in * mult;
        (num / den) as u64
    }
    fn estimated_give(&self, pool: &LiquidityPool, out_token: &Token, out_amount: u64) -> u64 {
        let (mult, modifier) = fee_mods(pool.pool_fee_percent);
        let (r_out, r_in) = corresponding_reserves(pool, out_token);
        let out_amt = out_amount as u128;
        if out_amt >= r_out {
            return u64::MAX; // out of range; caller's responsibility to guard
        }
        let num = out_amt * r_in * mult;
        let den = (r_out - out_amt) * modifier;
        ((num / den) + 1) as u64
    }
    fn price_impact_percent(&self, pool: &LiquidityPool, in_token: &Token, in_amount: u64) -> f64 {
        let (mult, modifier) = fee_mods(pool.pool_fee_percent);
        let (r_in, r_out) = corresponding_reserves(pool, in_token);
        // Use f64 throughout to avoid u128 overflow on large reserve × amount products.
        let in_amt = in_amount as f64;
        let r_in_f = r_in as f64;
        let r_out_f = r_out as f64;
        let mult_f = mult as f64;
        let mod_f = modifier as f64;
        let out_num = in_amt * mod_f * r_out_f;
        let out_den = in_amt * mod_f + r_in_f * mult_f;
        let pi_num = r_out_f * in_amt * out_den * mod_f - out_num * r_in_f * mult_f;
        let pi_den = r_out_f * in_amt * out_den * mult_f;
        pi_num * 100.0 / pi_den
    }
    fn swap_order_fees(&self) -> Vec<SwapFee> {
        vec![
            SwapFee {
                id: "batcherFee".into(),
                title: "Batcher Fee".into(),
                description: "Paid to the off-chain batcher to process the order.".into(),
                value: BATCHER_FEE_LOVELACE,
                is_returned: false,
            },
            SwapFee {
                id: "deposit".into(),
                title: "Deposit".into(),
                description: "Held as min-UTxO ADA; returned when the order is processed or cancelled.".into(),
                value: DEPOSIT_LOVELACE,
                is_returned: true,
            },
        ]
    }
    fn build_swap_order(&self, pool: &LiquidityPool, params: &SwapParams) -> Result<Vec<PayToAddress>> {
        if params.swap_in_amount == 0 {
            return Err(anyhow!("swap_in_amount must be > 0"));
        }
        // Validate that swap_in_token is one of the pool's assets.
        let in_id = token_identifier(&params.swap_in_token);
        let a_id = token_identifier(&pool.asset_a);
        let b_id = token_identifier(&pool.asset_b);
        if in_id != a_id && in_id != b_id {
            return Err(anyhow!("swap_in_token is not in this pool"));
        }

        // LP token name = the portion of pool_id AFTER the 56-char LP policy prefix.
        if pool.pool_id.len() < 56 || !pool.pool_id.starts_with(LP_TOKEN_POLICY_ID) {
            return Err(anyhow!(
                "pool.pool_id must start with the V2 LP policy ({}) — got `{}`",
                LP_TOKEN_POLICY_ID, pool.pool_id
            ));
        }
        let lp_name_hex = &pool.pool_id[56..];

        let direction = compute_direction(&params.swap_in_token, &params.swap_out_token);

        let datum = build_v2_order_datum(
            &params.sender.payment_key_hash,
            &params.receiver,
            LP_TOKEN_POLICY_ID,
            lp_name_hex,
            direction,
            params.swap_in_amount,
            params.min_receive,
            params.kill_on_failed,
            BATCHER_FEE_LOVELACE,
        )?;

        let order_address = script_and_stake_to_base_address(
            ORDER_SCRIPT_HASH,
            params.sender.staking_key_hash.as_deref().ok_or_else(|| {
                anyhow!("sender address has no staking credential (required for V2 order)")
            })?,
        )?;

        // Output lovelace = batcher + deposit + (swap-in if it's ADA).
        let mut lovelace = BATCHER_FEE_LOVELACE + DEPOSIT_LOVELACE;
        let mut assets: Vec<AssetAmount> = Vec::new();
        match &params.swap_in_token {
            Token::Lovelace => lovelace += params.swap_in_amount,
            Token::Asset(a) => {
                assets.push(AssetAmount {
                    unit: format!("{}{}", a.policy_id, a.name_hex),
                    quantity: params.swap_in_amount,
                });
            }
        }
        let mut all_assets = vec![AssetAmount { unit: "lovelace".into(), quantity: lovelace }];
        all_assets.append(&mut assets);

        Ok(vec![PayToAddress {
            address: order_address,
            address_type: AddressType::Contract,
            assets: all_assets,
            datum: Some(datum.to_cbor_hex()?),
            is_inline_datum: false,
            spend_utxos: vec![],
        }])
    }
    fn build_cancel_order(&self, order_utxos: &[Utxo], return_address: &str) -> Result<Vec<PayToAddress>> {
        let order_utxo = order_utxos
            .iter()
            .find(|u| payment_credential_is_order_script(&u.address))
            .ok_or_else(|| anyhow!("no UTxO at the V2 order script address"))?;

        // Return all assets of the order UTxO to the caller.
        let mut assets: Vec<AssetAmount> = Vec::with_capacity(order_utxo.amount.len());
        for unit in &order_utxo.amount {
            let qty = unit.quantity.parse::<u64>()
                .map_err(|e| anyhow!("order utxo asset quantity not u64: {}", e))?;
            assets.push(AssetAmount { unit: unit.unit.clone(), quantity: qty });
        }

        Ok(vec![PayToAddress {
            address: return_address.to_string(),
            address_type: AddressType::Base,
            assets,
            datum: None,
            is_inline_datum: false,
            spend_utxos: vec![SpendUtxo {
                utxo: order_utxo.clone(),
                redeemer: Some(CANCEL_REDEEMER.to_string()),
                validator: Some(PlutusScript {
                    version: PlutusVersion::V2,
                    cbor_hex: ORDER_SCRIPT_CBOR_HEX.to_string(),
                }),
                validator_reference: Some(UtxoRef {
                    tx_hash: ORDER_SCRIPT_REF_UTXO_TX.to_string(),
                    output_index: ORDER_SCRIPT_REF_UTXO_INDEX,
                    script_hash: ORDER_SCRIPT_HASH.to_string(),
                }),
                signer: Some(return_address.to_string()),
            }],
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Asset, LiquidityPool, Token};

    fn ada_token_pool(reserve_ada: u64, reserve_token: u64, fee_pct: f64) -> LiquidityPool {
        LiquidityPool {
            dex_identifier: "MinswapV2".into(),
            asset_a: Token::Lovelace,
            asset_b: Token::Asset(Asset::new(
                "f13ac4d66b3ee19a6aa0f2a22298737bd907cc9512166f2fc971b527",
                "535452494b45", 0,
            )),
            reserve_a: reserve_ada,
            reserve_b: reserve_token,
            address: "addr1...".into(),
            pool_id: format!("{}{}", "f5808c2c990d86da54bfc97d89cee6efa20cd8461616359478d96b4c",
                                     "7dd6988c5a86693c76aeec1ea94afa41770be0de21a775ca7a2a1eabdb6a0171"),
            pool_fee_percent: fee_pct,
            total_lp_tokens: 0,
        }
    }

    fn dex() -> MinswapV2 {
        MinswapV2::new(crate::KupoApi::new("http://localhost:1442"))
    }

    /// estimated_receive: ada→token swap.
    ///   formula: (in * fee_mod * out_reserve) / (in * fee_mod + in_reserve * fee_mult)
    ///   reserveA = 1_000_000_000_000  (1M ADA in lovelace)
    ///   reserveB = 5_000_000_000_000 (token)
    ///   in = 100_000_000 (100 ADA); fee 0.3% → fee_mod = 9970
    ///   numerator   = 100_000_000 * 9970 * 5_000_000_000_000 = 4_985_000_000_000_000_000_000_000
    ///   denominator = 100_000_000 * 9970 + 1_000_000_000_000 * 10000 = 10_000_997_000_000_000
    ///   = 498_450_304
    #[test]
    fn estimated_receive_constant_product_with_fee() {
        let pool = ada_token_pool(1_000_000_000_000, 5_000_000_000_000, 0.3);
        let got = dex().estimated_receive(&pool, &Token::Lovelace, 100_000_000);
        assert_eq!(got, 498_450_304);
    }

    /// estimated_give: inverse — to receive `out` of B, how much A in?
    ///   formula: (out * in_reserve * fee_mult) / ((out_reserve - out) * fee_mod) + 1
    ///   Choosing out = 498_450_304 (the round-trip from above) should return 100_000_000.
    #[test]
    fn estimated_give_round_trip() {
        let pool = ada_token_pool(1_000_000_000_000, 5_000_000_000_000, 0.3);
        let out = 498_450_304u64;
        let got = dex().estimated_give(&pool, &Token::Asset(match &pool.asset_b {
            Token::Asset(a) => a.clone(), _ => unreachable!(),
        }), out);
        assert_eq!(got, 100_000_000);
    }

    #[test]
    fn price_impact_is_positive_for_nontrivial_trade() {
        let pool = ada_token_pool(1_000_000_000_000, 5_000_000_000_000, 0.3);
        let pi = dex().price_impact_percent(&pool, &Token::Lovelace, 100_000_000);
        assert!(pi > 0.0 && pi < 1.0, "price impact = {}", pi);
    }

    #[test]
    fn build_cancel_order_picks_the_order_utxo_and_emits_redeemer() {
        use crate::models::Utxo;
        use crate::dex::minswap_v2_swap::{CANCEL_REDEEMER, ORDER_SCRIPT_CBOR_HEX};
        use crate::requests::PlutusVersion;

        // Two UTxOs — one at the order script address, one at an unrelated base address.
        let order_addr = "addr1z8p79rpkcdz8x9d6tft0x0dx5mwuzac2sa4gm8cvkw5hcnzr7sjjnw3fd7elhw73fqtcjae3yxd9xwwn2x265mnadv3qhj56am".to_string();
        let unrelated  = "addr1qyfd4vf3pwalnfxucjut2xx653s9ukguwnlrnjjq4qvld76r7sjjnw3fd7elhw73fqtcjae3yxd9xwwn2x265mnadv3qyun95l".to_string();

        let make_utxo = |address: String, ada: u64| Utxo {
            address,
            tx_hash: "00".repeat(32),
            tx_index: 0,
            output_index: 0,
            amount: vec![crate::models::Unit { unit: "lovelace".into(), quantity: ada.to_string() }],
            block: String::new(),
            data_hash: Some("ab".repeat(32)),
            inline_datum: None,
            reference_script_hash: None,
            datum_type: None,
        };

        let utxos = vec![
            make_utxo(unrelated.clone(), 5_000_000),
            make_utxo(order_addr.clone(), 2_004_000_000),
        ];

        let pays = dex().build_cancel_order(&utxos, &unrelated).expect("cancel build");
        assert_eq!(pays.len(), 1);
        let p = &pays[0];
        assert_eq!(p.address, unrelated, "funds return to the caller");
        assert_eq!(p.spend_utxos.len(), 1);
        let s = &p.spend_utxos[0];
        assert_eq!(s.utxo.address, order_addr);
        assert_eq!(s.redeemer.as_deref(), Some(CANCEL_REDEEMER));
        let v = s.validator.as_ref().expect("validator");
        assert_eq!(v.version, PlutusVersion::V2);
        assert_eq!(v.cbor_hex, ORDER_SCRIPT_CBOR_HEX);
        assert_eq!(s.signer.as_deref(), Some(unrelated.as_str()));
        assert_eq!(
            s.validator_reference.as_ref().map(|r| (r.tx_hash.as_str(), r.output_index)),
            Some(("cf4ecddde0d81f9ce8fcc881a85eb1f8ccdaf6807f03fea4cd02da896a621776", 0))
        );
        assert_eq!(
            s.validator_reference.as_ref().map(|r| r.script_hash.as_str()),
            Some(ORDER_SCRIPT_HASH)
        );
    }

    #[test]
    fn build_cancel_order_errors_when_no_order_utxo() {
        use crate::models::Utxo;
        let unrelated = "addr1qyfd4vf3pwalnfxucjut2xx653s9ukguwnlrnjjq4qvld76r7sjjnw3fd7elhw73fqtcjae3yxd9xwwn2x265mnadv3qyun95l".to_string();
        let other = Utxo {
            address: unrelated.clone(),
            tx_hash: "00".repeat(32),
            tx_index: 0,
            output_index: 0,
            amount: vec![crate::models::Unit { unit: "lovelace".into(), quantity: "5000000".into() }],
            block: String::new(),
            data_hash: None,
            inline_datum: None,
            reference_script_hash: None,
            datum_type: None,
        };
        assert!(dex().build_cancel_order(&[other], &unrelated).is_err());
    }
}
