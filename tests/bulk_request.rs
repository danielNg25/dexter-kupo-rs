use dexter_kupo_rs::{
    dex::minswap_v2::MinswapV2,
    BulkSwapRequest, KupoApi,
};

#[test]
fn empty_bundle_errors() {
    let dex = MinswapV2::new(KupoApi::new("http://localhost:1442"));
    let err = BulkSwapRequest::new(&dex)
        .for_pool(crate_test::test_pool())
        .with_return_address("addr1q8n0za95rmvwt45mvxc6lpedh5wx0jl9pyuyfufgzslfdg7tkywsce2nu7xc2pukfsl08x9wlmlnka59c5xcxjamsgfsnyaqug")
        .expect("addr")
        .build()
        .unwrap_err();
    assert!(format!("{err:#}").contains("empty bundle"));
}

#[test]
fn missing_return_address_errors() {
    let dex = MinswapV2::new(KupoApi::new("http://localhost:1442"));
    // for_pool is set; skip with_return_address. add_cancel with a synthetic utxo.
    let err = BulkSwapRequest::new(&dex)
        .for_pool(crate_test::test_pool())
        .add_cancel(crate_test::synthetic_order_utxo())
        .build()
        .unwrap_err();
    assert!(format!("{err:#}").contains("return_address"));
}

#[test]
fn missing_pool_errors() {
    let dex = MinswapV2::new(KupoApi::new("http://localhost:1442"));
    // Skip for_pool entirely.
    let err = BulkSwapRequest::new(&dex)
        .with_return_address("addr1q8n0za95rmvwt45mvxc6lpedh5wx0jl9pyuyfufgzslfdg7tkywsce2nu7xc2pukfsl08x9wlmlnka59c5xcxjamsgfsnyaqug")
        .expect("addr")
        .add_cancel(crate_test::synthetic_order_utxo())
        .build()
        .unwrap_err();
    assert!(format!("{err:#}").contains("pool not set"));
}

#[test]
fn bulk_of_one_swap_matches_single_swap_request() {
    use dexter_kupo_rs::dex::DexSwap;
    let dex = MinswapV2::new(KupoApi::new("http://localhost:1442"));
    let params = crate_test::test_swap_params();
    let bulk = BulkSwapRequest::new(&dex)
        .for_pool(crate_test::test_pool())
        .with_return_address(crate_test::SENDER_BECH32).expect("addr")
        .add_swap(params.clone())
        .build()
        .expect("bulk build");
    assert_eq!(bulk.cancels.len(), 0);
    assert_eq!(bulk.new_orders.len(), 1);

    let single = dex.build_swap_order(&crate_test::test_pool(), &params).expect("single build");
    assert_eq!(bulk.new_orders[0].datum, single[0].datum,
               "bulk-of-1 swap must produce identical datum to single SwapRequest");
    assert_eq!(bulk.new_orders[0].address, single[0].address,
               "bulk-of-1 swap must produce identical script address");
    assert_eq!(bulk.new_orders[0].assets.len(), single[0].assets.len(),
               "bulk-of-1 swap must produce same number of asset entries");
    assert!(bulk.new_orders[0].spend_utxos.is_empty(),
            "bulk swap output must have no spend_utxos");
}

#[test]
fn bulk_of_one_cancel_matches_single_cancel_request() {
    use dexter_kupo_rs::dex::DexSwap;
    let dex = MinswapV2::new(KupoApi::new("http://localhost:1442"));
    let order_utxo = crate_test::synthetic_order_utxo();
    let bulk = BulkSwapRequest::new(&dex)
        .for_pool(crate_test::test_pool())
        .with_return_address(crate_test::SENDER_BECH32).expect("addr")
        .add_cancel(order_utxo.clone())
        .build()
        .expect("bulk build");
    assert_eq!(bulk.cancels.len(), 1);
    assert_eq!(bulk.new_orders.len(), 0);

    let single = dex.build_cancel_order(&[order_utxo], crate_test::SENDER_BECH32).expect("single cancel");
    assert_eq!(bulk.cancels[0].utxo.tx_hash, single[0].spend_utxos[0].utxo.tx_hash);
    assert_eq!(bulk.cancels[0].redeemer, single[0].spend_utxos[0].redeemer);
    assert_eq!(bulk.cancels[0].validator_reference, single[0].spend_utxos[0].validator_reference);
}

#[test]
fn bulk_of_one_update_has_cancel_plus_new_order() {
    let dex = MinswapV2::new(KupoApi::new("http://localhost:1442"));
    let bulk = BulkSwapRequest::new(&dex)
        .for_pool(crate_test::test_pool())
        .with_return_address(crate_test::SENDER_BECH32).expect("addr")
        .add_update(crate_test::synthetic_order_utxo(), crate_test::test_swap_params())
        .build()
        .expect("bulk build");
    assert_eq!(bulk.cancels.len(), 1);
    assert_eq!(bulk.new_orders.len(), 1);
}

#[test]
fn bulk_mixed_three_kinds() {
    let dex = MinswapV2::new(KupoApi::new("http://localhost:1442"));
    let bulk = BulkSwapRequest::new(&dex)
        .for_pool(crate_test::test_pool())
        .with_return_address(crate_test::SENDER_BECH32).expect("addr")
        .add_swap(crate_test::test_swap_params())
        .add_cancel(crate_test::synthetic_order_utxo())
        .add_update(crate_test::synthetic_order_utxo(), crate_test::test_swap_params())
        .build()
        .expect("bulk build");
    assert_eq!(bulk.cancels.len(), 2);    // 1 cancel + 1 update-cancel
    assert_eq!(bulk.new_orders.len(), 2); // 1 swap + 1 update-place
}

mod crate_test {
    use dexter_kupo_rs::{
        models::{Asset, LiquidityPool, Token, Unit, Utxo},
        requests::types::{OrderKind, SwapParams},
        address::decode_base_address,
    };

    pub const SENDER_BECH32: &str =
        "addr1q8n0za95gc5qvjlacckd72lx83gt9ntgvf00u6z87h7mlq6r7sjjnw3fd7elhw73fqtcjae3yxd9xwwn2x265mnadv3q7pn7ep";

    pub fn test_pool() -> LiquidityPool {
        LiquidityPool {
            dex_identifier: "MinswapV2".into(),
            asset_a: Token::Lovelace,
            asset_b: Token::Asset(Asset::new(
                "f13ac4d66b3ee19a6aa0f2a22298737bd907cc9512166f2fc971b527",
                "535452494b45", 0,
            )),
            reserve_a: 1_000_000_000_000,
            reserve_b: 5_000_000_000_000,
            address: "addr1...".into(),
            pool_id: format!(
                "{}{}",
                "f5808c2c990d86da54bfc97d89cee6efa20cd8461616359478d96b4c",
                "7dd6988c5a86693c76aeec1ea94afa41770be0de21a775ca7a2a1eabdb6a0171"
            ),
            pool_fee_percent: 0.3,
            total_lp_tokens: 0,
        }
    }

    pub fn test_swap_params() -> SwapParams {
        let addr = decode_base_address(SENDER_BECH32).expect("addr");
        SwapParams {
            sender: addr.clone(),
            receiver: addr,
            swap_in_token: Token::Lovelace,
            swap_out_token: Token::Asset(Asset::new(
                "f13ac4d66b3ee19a6aa0f2a22298737bd907cc9512166f2fc971b527",
                "535452494b45", 0,
            )),
            swap_in_amount: 100_000_000,
            min_receive: 400_000_000,
            kind: OrderKind::Limit,
            kill_on_failed: false,
            spend_utxos: vec![],
        }
    }

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
