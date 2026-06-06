use dexter_kupo_rs::dex::minswap_v2::MinswapV2;
use dexter_kupo_rs::models::{Unit, Utxo};
use dexter_kupo_rs::{CancelSwapRequest, KupoApi};

#[test]
fn cancel_request_smoke() {
    let dex = MinswapV2::new(KupoApi::new("http://localhost:1442"));

    let order_addr = "addr1z8p79rpkcdz8x9d6tft0x0dx5mwuzac2sa4gm8cvkw5hcnzr7sjjnw3fd7elhw73fqtcjae3yxd9xwwn2x265mnadv3qhj56am";
    let return_addr = "addr1qyfd4vf3pwalnfxucjut2xx653s9ukguwnlrnjjq4qvld76r7sjjnw3fd7elhw73fqtcjae3yxd9xwwn2x265mnadv3qyun95l";

    let order_utxo = Utxo {
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
    };

    let pays = CancelSwapRequest::new(&dex)
        .with_order_utxos(vec![order_utxo])
        .with_return_address(return_addr)
        .build()
        .unwrap();
    assert_eq!(pays[0].address, return_addr);
    assert_eq!(pays[0].spend_utxos[0].redeemer.as_deref(), Some("d87a80"));
}
