use dexter_kupo_rs::dex::minswap_v2::MinswapV2;
use dexter_kupo_rs::models::{Asset, LiquidityPool, Token};
use dexter_kupo_rs::{KupoApi, SwapRequest};

// NOTE: this is the sender bech32 whose decoded payment_key_hash matches the
// on-chain golden datum's canceller PKH (e6f174...) — verified in Task 13.
const SENDER_ADDR: &str = "addr1q8n0za95gc5qvjlacckd72lx83gt9ntgvf00u6z87h7mlq6r7sjjnw3fd7elhw73fqtcjae3yxd9xwwn2x265mnadv3q7pn7ep";
const GOLDEN_DATUM_HEX: &str = "d8799fd8799f581ce6f174b44628064bfdc62cdf2be63c50b2cd68625efe6847f5fdbf83ffd8799fd8799f581ce6f174b44628064bfdc62cdf2be63c50b2cd68625efe6847f5fdbf83ffd8799fd8799fd8799f581c43f42529ba296fb3fbbbd14817897731219a5339d35195aa6e7d6b22ffffffffd87980d8799fd8799f581ce6f174b44628064bfdc62cdf2be63c50b2cd68625efe6847f5fdbf83ffd8799fd8799fd8799f581c43f42529ba296fb3fbbbd14817897731219a5339d35195aa6e7d6b22ffffffffd87980d8799f581cf5808c2c990d86da54bfc97d89cee6efa20cd8461616359478d96b4c58207dd6988c5a86693c76aeec1ea94afa41770be0de21a775ca7a2a1eabdb6a0171ffd8799fd87a80d8799f1a77359400ff1a1460ae15d87980ff1a001e8480d87a80ff";

#[test]
fn swap_request_limit_path_reproduces_golden() {
    let dex = MinswapV2::new(KupoApi::new("http://localhost:1442"));
    let pool = LiquidityPool {
        dex_identifier: "MinswapV2".into(),
        asset_a: Token::Lovelace,
        asset_b: Token::Asset(Asset::new(
            "1111111111111111111111111111111111111111111111111111111c", "deadbeef", 0,
        )),
        reserve_a: 100_000_000_000_000,
        reserve_b: 100_000_000_000_000,
        address: "addr1...".into(),
        pool_id: "f5808c2c990d86da54bfc97d89cee6efa20cd8461616359478d96b4c7dd6988c5a86693c76aeec1ea94afa41770be0de21a775ca7a2a1eabdb6a0171".into(),
        pool_fee_percent: 0.3,
        total_lp_tokens: 0,
    };

    let pays = SwapRequest::new(&dex)
        .for_pool(pool)
        .with_swap_in_token(Token::Lovelace)
        .with_swap_in_amount(2_000_000_000)
        .with_minimum_receive(341_880_341)
        .with_sender_address(SENDER_ADDR).unwrap()
        .build()
        .unwrap();

    assert_eq!(pays.len(), 1);
    assert_eq!(pays[0].datum.as_deref(), Some(GOLDEN_DATUM_HEX));
    assert_eq!(pays[0].assets[0].quantity, 2_004_000_000);
}
