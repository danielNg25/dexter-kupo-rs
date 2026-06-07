use dexter_kupo_rs::dex::minswap_v2::MinswapV2;
use dexter_kupo_rs::models::{Asset, LiquidityPool, Token, Unit, Utxo};
use dexter_kupo_rs::{KupoApi, UpdateSwapRequest};

const SENDER_ADDR: &str = "addr1q8n0za95gc5qvjlacckd72lx83gt9ntgvf00u6z87h7mlq6r7sjjnw3fd7elhw73fqtcjae3yxd9xwwn2x265mnadv3q7pn7ep";

fn make_pool() -> LiquidityPool {
    LiquidityPool {
        dex_identifier: "MinswapV2".into(),
        asset_a: Token::Lovelace,
        asset_b: Token::Asset(Asset::new(
            "1111111111111111111111111111111111111111111111111111111c",
            "deadbeef",
            0,
        )),
        reserve_a: 100_000_000_000_000,
        reserve_b: 100_000_000_000_000,
        address: "addr1...".into(),
        pool_id: "f5808c2c990d86da54bfc97d89cee6efa20cd8461616359478d96b4c7dd6988c5a86693c76aeec1ea94afa41770be0de21a775ca7a2a1eabdb6a0171".into(),
        pool_fee_percent: 0.3,
        total_lp_tokens: 0,
    }
}

fn make_order_utxo() -> Utxo {
    let order_addr = "addr1z8p79rpkcdz8x9d6tft0x0dx5mwuzac2sa4gm8cvkw5hcnzr7sjjnw3fd7elhw73fqtcjae3yxd9xwwn2x265mnadv3qhj56am";
    Utxo {
        address: order_addr.to_string(),
        tx_hash: "00".repeat(32),
        tx_index: 0,
        output_index: 0,
        amount: vec![Unit { unit: "lovelace".into(), quantity: "2004000000".into() }],
        block: String::new(),
        data_hash: Some("ab".repeat(32)),
        inline_datum: None,
        reference_script_hash: None,
        datum_type: None,
    }
}

#[test]
fn update_request_builds_single_pay_to_address() {
    let dex = MinswapV2::new(KupoApi::new("http://localhost:1442"));

    let pays = UpdateSwapRequest::new(&dex)
        .for_pool(make_pool())
        .with_swap_in_token(Token::Lovelace)
        .with_swap_in_amount(2_000_000_000)
        .with_minimum_receive(341_880_341)
        .with_sender_address(SENDER_ADDR)
        .unwrap()
        .with_old_order_utxos(vec![make_order_utxo()])
        .build()
        .unwrap();

    // Length 1
    assert_eq!(pays.len(), 1);

    // Has a datum
    assert!(pays[0].datum.is_some(), "expected a datum on the new order output");

    // Has lovelace in assets
    let has_lovelace = pays[0].assets.iter().any(|a| a.unit == "lovelace" && a.quantity > 0);
    assert!(has_lovelace, "expected lovelace in output assets");

    // spend_utxos.len() == 1
    assert_eq!(pays[0].spend_utxos.len(), 1, "expected exactly 1 spend_utxo (the old order)");

    // spend_utxo redeemer is Some (cancel redeemer)
    assert!(
        pays[0].spend_utxos[0].redeemer.is_some(),
        "expected cancel redeemer on old order spend_utxo"
    );

    // spend_utxo validator_reference is Some
    assert!(
        pays[0].spend_utxos[0].validator_reference.is_some(),
        "expected validator_reference on old order spend_utxo"
    );
}

#[test]
fn update_request_errors_when_no_old_utxos() {
    let dex = MinswapV2::new(KupoApi::new("http://localhost:1442"));

    let result = UpdateSwapRequest::new(&dex)
        .for_pool(make_pool())
        .with_swap_in_token(Token::Lovelace)
        .with_swap_in_amount(2_000_000_000)
        .with_minimum_receive(341_880_341)
        .with_sender_address(SENDER_ADDR)
        .unwrap()
        // intentionally omit with_old_order_utxos
        .build();

    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("no old order UTxOs"),
        "expected 'no old order UTxOs' error message"
    );
}
