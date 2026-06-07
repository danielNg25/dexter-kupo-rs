//! Golden-vector test: reproduce the V2 limit-order datum from a real on-chain tx
//! byte-for-byte, using only the decoded parameters.
//!
//! Source tx: ddabf4cd690979b4c19a3c46117d3837966f35c0e1db8b59fd972cb7d8e3fca3
//! Order datum hash: 6128a1cb53233958ea9294c636d1aad066c2da141ac3d58190295a71f0e74fb5

use dexter_kupo_rs::plutus::PlutusData;

const GOLDEN_DATUM_HEX: &str = "d8799fd8799f581ce6f174b44628064bfdc62cdf2be63c50b2cd68625efe6847f5fdbf83ffd8799fd8799f581ce6f174b44628064bfdc62cdf2be63c50b2cd68625efe6847f5fdbf83ffd8799fd8799fd8799f581c43f42529ba296fb3fbbbd14817897731219a5339d35195aa6e7d6b22ffffffffd87980d8799fd8799f581ce6f174b44628064bfdc62cdf2be63c50b2cd68625efe6847f5fdbf83ffd8799fd8799fd8799f581c43f42529ba296fb3fbbbd14817897731219a5339d35195aa6e7d6b22ffffffffd87980d8799f581cf5808c2c990d86da54bfc97d89cee6efa20cd8461616359478d96b4c58207dd6988c5a86693c76aeec1ea94afa41770be0de21a775ca7a2a1eabdb6a0171ffd8799fd87a80d8799f1a77359400ff1a1460ae15d87980ff1a001e8480d87a80ff";

const SENDER_PKH:        &str = "e6f174b44628064bfdc62cdf2be63c50b2cd68625efe6847f5fdbf83";
const SENDER_STAKE_KH:   &str = "43f42529ba296fb3fbbbd14817897731219a5339d35195aa6e7d6b22";
const LP_TOKEN_POLICY:   &str = "f5808c2c990d86da54bfc97d89cee6efa20cd8461616359478d96b4c";
const LP_TOKEN_NAME:     &str = "7dd6988c5a86693c76aeec1ea94afa41770be0de21a775ca7a2a1eabdb6a0171";
const DIRECTION:         u64  = 1;
const SWAP_IN_AMOUNT:    i128 = 2_000_000_000;
const MIN_RECEIVE:       i128 = 341_880_341;
const BATCHER_FEE:       i128 = 2_000_000;

/// Build the 9-field V2 OrderDatum from raw params using only `PlutusData`.
fn build_v2_order_datum(
    sender_pkh: &str,
    sender_stake: &str,
    lp_policy: &str,
    lp_name: &str,
    direction: u64,
    swap_in_amount: i128,
    min_receive: i128,
    batcher_fee: i128,
) -> PlutusData {
    let pkh_bytes        = PlutusData::bytes_hex(sender_pkh).unwrap();
    let stake_bytes      = PlutusData::bytes_hex(sender_stake).unwrap();
    let lp_policy_bytes  = PlutusData::bytes_hex(lp_policy).unwrap();
    let lp_name_bytes    = PlutusData::bytes_hex(lp_name).unwrap();

    // Address(payment=pkh, stake=Some(key_hash)) — the wallet/address shape used
    // by both refund_receiver and success_receiver in the V2 datum.
    let wallet_address = PlutusData::Constr(0, vec![
        // payment credential = PubKey(pkh)
        PlutusData::Constr(0, vec![pkh_bytes.clone()]),
        // staking credential = Some(StakingHash(PubKey(stake)))
        PlutusData::Constr(0, vec![
            PlutusData::Constr(0, vec![
                PlutusData::Constr(0, vec![stake_bytes.clone()]),
            ]),
        ]),
    ]);

    PlutusData::Constr(0, vec![
        // 0: canceller — PubKey credential
        PlutusData::Constr(0, vec![pkh_bytes.clone()]),
        // 1: refund_receiver
        wallet_address.clone(),
        // 2: refund_receiver_datum = NoDatum (Constr 0 empty)
        PlutusData::Constr(0, vec![]),
        // 3: success_receiver
        wallet_address,
        // 4: success_receiver_datum = NoDatum
        PlutusData::Constr(0, vec![]),
        // 5: lp_asset = (policy, name)
        PlutusData::Constr(0, vec![lp_policy_bytes, lp_name_bytes]),
        // 6: step = SwapExactIn { direction, SpecificAmount(amount), min_receive, killable=False }
        PlutusData::Constr(0, vec![
            PlutusData::Constr(direction, vec![]),
            PlutusData::Constr(0, vec![PlutusData::Int(swap_in_amount)]),
            PlutusData::Int(min_receive),
            PlutusData::Constr(0, vec![]),
        ]),
        // 7: max_batcher_fee
        PlutusData::Int(batcher_fee),
        // 8: expired_setting_opt = None (Constr 1 empty)
        PlutusData::Constr(1, vec![]),
    ])
}

#[test]
fn v2_order_datum_matches_onchain_golden() {
    let datum = build_v2_order_datum(
        SENDER_PKH, SENDER_STAKE_KH, LP_TOKEN_POLICY, LP_TOKEN_NAME,
        DIRECTION, SWAP_IN_AMOUNT, MIN_RECEIVE, BATCHER_FEE,
    );
    let got = datum.to_cbor_hex().expect("encode failed");
    assert_eq!(got, GOLDEN_DATUM_HEX, "datum CBOR diverges from on-chain golden");
}

use dexter_kupo_rs::address::decode_base_address;
use dexter_kupo_rs::dex::minswap_v2::MinswapV2;
use dexter_kupo_rs::dex::DexSwap;
use dexter_kupo_rs::models::{Asset, LiquidityPool, Token};
use dexter_kupo_rs::{KupoApi, OrderKind, SwapParams};

// Sender address whose payment key hash is e6f174... (matches the golden datum PKH)
// and whose stake key hash is 43f425... (same stake as used for the order address derivation).
const SENDER_ADDR: &str = "addr1q8n0za95gc5qvjlacckd72lx83gt9ntgvf00u6z87h7mlq6r7sjjnw3fd7elhw73fqtcjae3yxd9xwwn2x265mnadv3q7pn7ep";
const ORDER_ADDR:  &str = "addr1z8p79rpkcdz8x9d6tft0x0dx5mwuzac2sa4gm8cvkw5hcnzr7sjjnw3fd7elhw73fqtcjae3yxd9xwwn2x265mnadv3qhj56am";

#[test]
fn build_swap_order_reproduces_onchain_golden() {
    // Sender's full address from the golden tx.
    let sender   = decode_base_address(SENDER_ADDR).unwrap();
    let receiver = sender.clone();

    // Pool: ADA / SOME_TOKEN (B unit hex used here is arbitrary — irrelevant
    // to the datum; only the LP token name in pool_id matters for the datum,
    // plus the direction calculation, which depends on the token unit
    // (ADA "" < any hex), so any non-empty token unit reproduces direction=1).
    let pool = LiquidityPool {
        dex_identifier: "MinswapV2".into(),
        asset_a: Token::Lovelace,
        asset_b: Token::Asset(Asset::new(
            "1111111111111111111111111111111111111111111111111111111c",
            "deadbeef", 0,
        )),
        reserve_a: 100_000_000_000_000,
        reserve_b: 100_000_000_000_000,
        address: "addr1...".into(),
        // pool_id = LP_TOKEN_POLICY || LP_TOKEN_NAME (matches the V2 reader's behaviour)
        pool_id: format!("{}{}", LP_TOKEN_POLICY, LP_TOKEN_NAME),
        pool_fee_percent: 0.3,
        total_lp_tokens: 0,
    };

    let params = SwapParams {
        sender,
        receiver,
        swap_in_token: Token::Lovelace,
        swap_out_token: pool.asset_b.clone(),
        swap_in_amount: SWAP_IN_AMOUNT as u64,
        min_receive: MIN_RECEIVE as u64,
        kind: OrderKind::Limit,
        kill_on_failed: false,
        spend_utxos: vec![],
    };

    let dex = MinswapV2::new(KupoApi::new("http://localhost:1442"));
    let pays = dex.build_swap_order(&pool, &params).expect("build failed");
    assert_eq!(pays.len(), 1);

    let p = &pays[0];
    assert_eq!(p.address, ORDER_ADDR);
    assert_eq!(p.is_inline_datum, false);
    assert_eq!(p.datum.as_deref(), Some(GOLDEN_DATUM_HEX),
               "datum CBOR diverges from on-chain golden");

    // One lovelace amount = swap_in (2000 ADA) + batcher (2) + deposit (2)
    assert_eq!(p.assets.len(), 1, "ADA-in swap should have a single lovelace amount");
    assert_eq!(p.assets[0].unit, "lovelace");
    assert_eq!(p.assets[0].quantity, 2_000_000_000 + 2_000_000 + 2_000_000);
}
