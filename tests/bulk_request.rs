use dexter_kupo_rs::{
    dex::minswap_v2::MinswapV2,
    BulkSwapRequest, KupoApi,
};

#[test]
fn empty_bundle_errors() {
    let dex = MinswapV2::new(KupoApi::new("http://localhost:1442"));
    let err = BulkSwapRequest::new(&dex)
        .with_return_address("addr1q8n0za95rmvwt45mvxc6lpedh5wx0jl9pyuyfufgzslfdg7tkywsce2nu7xc2pukfsl08x9wlmlnka59c5xcxjamsgfsnyaqug")
        .expect("addr")
        .build()
        .unwrap_err();
    assert!(format!("{err:#}").contains("empty bundle"));
}

#[test]
fn missing_return_address_errors() {
    let dex = MinswapV2::new(KupoApi::new("http://localhost:1442"));
    // Skip with_return_address. add_cancel with a synthetic utxo.
    let err = BulkSwapRequest::new(&dex)
        .add_cancel(crate_test::synthetic_order_utxo())
        .build()
        .unwrap_err();
    assert!(format!("{err:#}").contains("return_address"));
}

mod crate_test {
    use dexter_kupo_rs::models::{Unit, Utxo};
    pub fn synthetic_order_utxo() -> Utxo {
        Utxo {
            address: "addr1z8p79rpkcdz8x9d6tft0x0dx5mwuzac2sa4gm8cvkw5hcn8ftv3526r2y8z8rnlvay76nhpdp9pdekzvd3rrrql08qqssmhdxz".into(),
            tx_hash: "aa".repeat(32),
            tx_index: 0,
            output_index: 0,
            amount: vec![Unit { unit: "lovelace".into(), quantity: "5000000".into() }],
            block: String::new(),
            data_hash: None,
            inline_datum: None,
            reference_script_hash: None,
            datum_type: None,
        }
    }
}
