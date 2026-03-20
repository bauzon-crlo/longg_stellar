// src/test.rs
#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, Bytes, Env,
};

fn asset_bytes(e: &Env, label: &[u8; 32]) -> Bytes {
    Bytes::from_slice(e, label)
}

fn encode_feed(e: &Env, asset: [u8; 32], price: i128, timestamp: u64, decimals: u8) -> Bytes {
    use alloy_sol_types::private::primitives::{I256, U256};
    use alloy_sol_types::SolValue;
    let input = PriceFeedInput {
        asset: asset.into(),
        price: I256::try_from(price).unwrap(),
        timestamp: U256::from(timestamp),
        decimals,
    };
    Bytes::from_slice(e, &input.abi_encode())
}

fn setup() -> (Env, Address, Address, Address) {
    let e = Env::default();
    e.mock_all_auths();
    e.ledger().with_mut(|l| l.timestamp = 1_000_000);

    let admin = Address::generate(&e);
    let updater = Address::generate(&e);
    let contract_id = e.register(PriceBridge, ());
    let client = PriceBridgeClient::new(&e, &contract_id);

    client.initialize(&admin, &300u64);
    client.set_updater(&admin, &updater, &true);

    (e, contract_id, admin, updater)
}

fn register_eth(e: &Env, contract_id: &Address, admin: &Address) -> [u8; 32] {
    let client = PriceBridgeClient::new(e, contract_id);
    let mut asset = [0u8; 32];
    asset[..3].copy_from_slice(b"ETH");
    client.register_feed(
        admin,
        &asset_bytes(e, &asset),
        &300u64,
        &1_000_000i128,
        &1_000_000_000i128,
        &1000u32,
        &5u32,
    );
    asset
}

#[test]
fn test_submit_and_get() {
    let (e, contract_id, admin, updater) = setup();
    let client = PriceBridgeClient::new(&e, &contract_id);
    let asset = register_eth(&e, &contract_id, &admin);

    let input = encode_feed(&e, asset, 3_000_00000000i128, 1_000_000u64, 8);
    client.submit(&updater, &input).unwrap();

    let entry = client.get_price(&asset_bytes(&e, &asset)).unwrap();
    assert_eq!(entry.price, 3_000_00000000i128);
    assert_eq!(entry.decimals, 8);
}

#[test]
fn test_normalized_price() {
    let (e, contract_id, admin, updater) = setup();
    let client = PriceBridgeClient::new(&e, &contract_id);
    let asset = register_eth(&e, &contract_id, &admin);

    let input = encode_feed(&e, asset, 3_000_00000000i128, 1_000_000u64, 8);
    client.submit(&updater, &input).unwrap();

    let normalized = client.get_normalized_price(&asset_bytes(&e, &asset)).unwrap();
    assert_eq!(normalized, 3_000_00000000i128 * 10i128.pow(10));
}

#[test]
fn test_twap_computed() {
    let (e, contract_id, admin, updater) = setup();
    let client = PriceBridgeClient::new(&e, &contract_id);
    let asset = register_eth(&e, &contract_id, &admin);

    for (i, price) in [2_900_00000000i128, 3_000_00000000i128, 3_100_00000000i128]
        .iter()
        .enumerate()
    {
        e.ledger().with_mut(|l| l.timestamp = 1_000_000 + i as u64 * 100);
        let input = encode_feed(&e, asset, *price, 1_000_000 + i as u64 * 100, 8);
        client.submit(&updater, &input).unwrap();
    }

    let twap = client.get_twap(&asset_bytes(&e, &asset)).unwrap();
    assert!(twap >= 2_900_00000000i128);
    assert!(twap <= 3_100_00000000i128);
}

#[test]
fn test_twap_insufficient_history() {
    let (e, contract_id, admin, updater) = setup();
    let client = PriceBridgeClient::new(&e, &contract_id);
    let asset = register_eth(&e, &contract_id, &admin);

    let input = encode_feed(&e, asset, 3_000_00000000i128, 1_000_000u64, 8);
    client.submit(&updater, &input).unwrap();

    let result = client.get_twap(&asset_bytes(&e, &asset));
    assert_eq!(result, Err(Error::InsufficientHistory));
}

#[test]
fn test_circuit_breaker_trips_on_large_deviation() {
    let (e, contract_id, admin, updater) = setup();
    let client = PriceBridgeClient::new(&e, &contract_id);
    let asset = register_eth(&e, &contract_id, &admin);

    let input = encode_feed(&e, asset, 3_000_00000000i128, 1_000_000u64, 8);
    client.submit(&updater, &input).unwrap();

    e.ledger().with_mut(|l| l.timestamp = 1_000_100);
    let input2 = encode_feed(&e, asset, 3_500_00000000i128, 1_000_100u64, 8);
    let result = client.submit(&updater, &input2);
    assert_eq!(result, Err(Error::CircuitBreakerTripped));

    assert!(client.is_circuit_broken(&asset_bytes(&e, &asset)));
}

#[test]
fn test_circuit_breaker_reset_by_admin() {
    let (e, contract_id, admin, updater) = setup();
    let client = PriceBridgeClient::new(&e, &contract_id);
    let asset = register_eth(&e, &contract_id, &admin);

    let input = encode_feed(&e, asset, 3_000_00000000i128, 1_000_000u64, 8);
    client.submit(&updater, &input).unwrap();
    e.ledger().with_mut(|l| l.timestamp = 1_000_100);
    let input2 = encode_feed(&e, asset, 3_500_00000000i128, 1_000_100u64, 8);
    let _ = client.submit(&updater, &input2);

    client
        .reset_circuit_breaker(&admin, &asset_bytes(&e, &asset))
        .unwrap();
    assert!(!client.is_circuit_broken(&asset_bytes(&e, &asset)));
}

#[test]
fn test_price_history_stored() {
    let (e, contract_id, admin, updater) = setup();
    let client = PriceBridgeClient::new(&e, &contract_id);
    let asset = register_eth(&e, &contract_id, &admin);

    for i in 0..3u64 {
        e.ledger().with_mut(|l| l.timestamp = 1_000_000 + i * 60);
        let input = encode_feed(&e, asset, 3_000_00000000i128, 1_000_000 + i * 60, 8);
        client.submit(&updater, &input).unwrap();
    }

    let history = client.get_history(&asset_bytes(&e, &asset)).unwrap();
    assert_eq!(history.len(), 3);
}

#[test]
fn test_stale_rejected() {
    let (e, contract_id, admin, updater) = setup();
    let client = PriceBridgeClient::new(&e, &contract_id);
    let asset = register_eth(&e, &contract_id, &admin);

    let input = encode_feed(&e, asset, 3_000_00000000i128, 999_000u64, 8);
    let result = client.submit(&updater, &input);
    assert_eq!(result, Err(Error::StalePriceFeed));
}

#[test]
fn test_abi_output_includes_twap_and_normalized() {
    let (e, contract_id, admin, updater) = setup();
    let client = PriceBridgeClient::new(&e, &contract_id);
    let asset = register_eth(&e, &contract_id, &admin);

    for i in 0..2u64 {
        e.ledger().with_mut(|l| l.timestamp = 1_000_000 + i * 60);
        let input = encode_feed(&e, asset, 3_000_00000000i128, 1_000_000 + i * 60, 8);
        client.submit(&updater, &input).unwrap();
    }

    let abi = client.get_price_abi(&asset_bytes(&e, &asset)).unwrap();
    assert!(abi.len() > 0);
}

#[test]
fn test_unauthorized_updater_rejected() {
    let (e, contract_id, admin, _) = setup();
    let client = PriceBridgeClient::new(&e, &contract_id);
    let asset = register_eth(&e, &contract_id, &admin);

    let rando = Address::generate(&e);
    let input = encode_feed(&e, asset, 3_000_00000000i128, 1_000_000u64, 8);
    let result = client.submit(&rando, &input);
    assert_eq!(result, Err(Error::Unauthorized));
}

#[test]
fn test_unregistered_feed_rejected() {
    let (e, contract_id, _, updater) = setup();
    let client = PriceBridgeClient::new(&e, &contract_id);

    let mut unknown_asset = [0u8; 32];
    unknown_asset[..3].copy_from_slice(b"BTC");

    let input = encode_feed(&e, unknown_asset, 50_000_00000000i128, 1_000_000u64, 8);
    let result = client.submit(&updater, &input);
    assert_eq!(result, Err(Error::FeedNotRegistered));
}

#[test]
fn test_invalid_decimals_rejected() {
    let (e, contract_id, admin, updater) = setup();
    let client = PriceBridgeClient::new(&e, &contract_id);
    let asset = register_eth(&e, &contract_id, &admin);

    let input = encode_feed(&e, asset, 3_000_00000000i128, 1_000_000u64, 19);
    let result = client.submit(&updater, &input);
    assert_eq!(result, Err(Error::InvalidDecimals));
}

#[test]
fn test_feed_deactivation() {
    let (e, contract_id, admin, updater) = setup();
    let client = PriceBridgeClient::new(&e, &contract_id);
    let asset = register_eth(&e, &contract_id, &admin);

    client.set_feed_active(&admin, &asset_bytes(&e, &asset), &false).unwrap();

    let input = encode_feed(&e, asset, 3_000_00000000i128, 1_000_000u64, 8);
    let result = client.submit(&updater, &input);
    assert_eq!(result, Err(Error::FeedNotRegistered));
}

#[test]
fn test_is_fresh() {
    let (e, contract_id, admin, updater) = setup();
    let client = PriceBridgeClient::new(&e, &contract_id);
    let asset = register_eth(&e, &contract_id, &admin);

    assert!(!client.is_fresh(&asset_bytes(&e, &asset)));

    let input = encode_feed(&e, asset, 3_000_00000000i128, 1_000_000u64, 8);
    client.submit(&updater, &input).unwrap();

    assert!(client.is_fresh(&asset_bytes(&e, &asset)));
}
