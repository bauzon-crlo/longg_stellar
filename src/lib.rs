// src/lib.rs
#![no_std]
extern crate alloc;

use alloy_sol_types::{sol, SolValue};
use soroban_sdk::{
    contract, contracterror, contractimpl, contractmeta, contracttype, Address, Bytes, Env,
};

contractmeta!(
    key = "Description",
    val = "PriceBridge: EVM-compatible Chainlink-style oracle relay for Soroban with TWAP, circuit breaker, and price normalization"
);

#[contracterror]
#[repr(u32)]
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum Error {
    Unauthorized = 1,
    Decode = 2,
    StalePriceFeed = 3,
    AssetNotFound = 4,
    InvalidPrice = 5,
    InvalidDecimals = 6,
    FeedAlreadyExists = 7,
    FeedNotRegistered = 8,
    CircuitBreakerTripped = 9,
    InsufficientHistory = 10,
}

sol! {
    struct PriceFeedInput {
        bytes32 asset;
        int256 price;
        uint256 timestamp;
        uint8 decimals;
    }

    struct PriceFeedOutput {
        bytes32 asset;
        int256 price;
        uint256 timestamp;
        uint8 decimals;
        uint256 queried_at;
        int256 twap;
        int256 normalized;
    }
}

#[contracttype]
#[derive(Clone)]
pub struct PriceEntry {
    pub asset: Bytes,
    pub price: i128,
    pub timestamp: u64,
    pub decimals: u32,
    pub updated_at: u64,
    pub updater: Address,
    pub normalized: i128,
}

#[contracttype]
#[derive(Clone)]
pub struct PriceSnapshot {
    pub price: i128,
    pub timestamp: u64,
}

#[contracttype]
#[derive(Clone)]
pub struct FeedConfig {
    pub max_staleness: u64,
    pub min_price: i128,
    pub max_price: i128,
    pub active: bool,
    pub max_deviation_bps: u32,
    pub twap_window: u32,
}

#[contracttype]
#[derive(Clone)]
pub struct DefiState {
    pub circuit_broken: bool,
    pub last_price: i128,
    pub history: soroban_sdk::Vec<PriceSnapshot>,
}

#[contracttype]
pub enum StorageKey {
    Admin,
    Price(Bytes),
    FeedConfig(Bytes),
    DefiState(Bytes),
    Updater(Address),
}

#[contract]
pub struct PriceBridge;

#[contractimpl]
impl PriceBridge {
    pub fn initialize(e: Env, admin: Address, _max_staleness: u64) {
        admin.require_auth();
        e.storage().instance().set(&StorageKey::Admin, &admin);
    }

    pub fn register_feed(
        e: Env,
        caller: Address,
        asset: Bytes,
        max_staleness: u64,
        min_price: i128,
        max_price: i128,
        max_deviation_bps: u32,
        twap_window: u32,
    ) -> Result<(), Error> {
        caller.require_auth();
        Self::require_admin(&e, &caller)?;

        if e.storage()
            .instance()
            .has(&StorageKey::FeedConfig(asset.clone()))
        {
            return Err(Error::FeedAlreadyExists);
        }

        let config = FeedConfig {
            max_staleness,
            min_price,
            max_price,
            active: true,
            max_deviation_bps,
            twap_window: twap_window.max(1).min(10),
        };

        let defi_state = DefiState {
            circuit_broken: false,
            last_price: 0,
            history: soroban_sdk::Vec::new(&e),
        };

        e.storage()
            .instance()
            .set(&StorageKey::FeedConfig(asset.clone()), &config);
        e.storage()
            .instance()
            .set(&StorageKey::DefiState(asset), &defi_state);

        Ok(())
    }

    pub fn set_updater(
        e: Env,
        caller: Address,
        updater: Address,
        allowed: bool,
    ) -> Result<(), Error> {
        caller.require_auth();
        Self::require_admin(&e, &caller)?;
        e.storage()
            .instance()
            .set(&StorageKey::Updater(updater), &allowed);
        Ok(())
    }

    pub fn set_feed_active(
        e: Env,
        caller: Address,
        asset: Bytes,
        active: bool,
    ) -> Result<(), Error> {
        caller.require_auth();
        Self::require_admin(&e, &caller)?;

        let mut config: FeedConfig = e
            .storage()
            .instance()
            .get(&StorageKey::FeedConfig(asset.clone()))
            .ok_or(Error::FeedNotRegistered)?;

        config.active = active;
        e.storage()
            .instance()
            .set(&StorageKey::FeedConfig(asset), &config);
        Ok(())
    }

    pub fn reset_circuit_breaker(
        e: Env,
        caller: Address,
        asset: Bytes,
    ) -> Result<(), Error> {
        caller.require_auth();
        Self::require_admin(&e, &caller)?;

        let mut state: DefiState = e
            .storage()
            .instance()
            .get(&StorageKey::DefiState(asset.clone()))
            .ok_or(Error::FeedNotRegistered)?;

        state.circuit_broken = false;
        e.storage()
            .instance()
            .set(&StorageKey::DefiState(asset), &state);
        Ok(())
    }

    pub fn submit(e: Env, caller: Address, input: Bytes) -> Result<(), Error> {
        caller.require_auth();
        Self::require_updater(&e, &caller)?;

        let mut buf = [0u8; 128];
        let len = input.len() as usize;
        input.copy_into_slice(&mut buf[..len]);

        // fix: abi_decode requires validate: bool as second argument
        let decoded = PriceFeedInput::abi_decode(&buf[..len], true)
            .map_err(|_| Error::Decode)?;

        let asset_bytes = Bytes::from_slice(&e, decoded.asset.as_slice());

        let config: FeedConfig = e
            .storage()
            .instance()
            .get(&StorageKey::FeedConfig(asset_bytes.clone()))
            .ok_or(Error::FeedNotRegistered)?;

        if !config.active {
            return Err(Error::FeedNotRegistered);
        }

        // fix: use try_into() instead of as_i128()
        let price: i128 = decoded.price.try_into().map_err(|_| Error::InvalidPrice)?;

        if price <= 0 {
            return Err(Error::InvalidPrice);
        }
        if config.min_price > 0 && price < config.min_price {
            return Err(Error::InvalidPrice);
        }
        if config.max_price > 0 && price > config.max_price {
            return Err(Error::InvalidPrice);
        }
        if decoded.decimals > 18 {
            return Err(Error::InvalidDecimals);
        }

        let now = e.ledger().timestamp();
        let timestamp = decoded.timestamp.to::<u64>();
        if now > timestamp && (now - timestamp) > config.max_staleness {
            return Err(Error::StalePriceFeed);
        }

        let mut state: DefiState = e
            .storage()
            .instance()
            .get(&StorageKey::DefiState(asset_bytes.clone()))
            .ok_or(Error::FeedNotRegistered)?;

        if state.circuit_broken {
            return Err(Error::CircuitBreakerTripped);
        }

        if state.last_price > 0 && config.max_deviation_bps > 0 {
            let diff = (price - state.last_price).abs();
            let deviation_bps = (diff * 10_000) / state.last_price;
            if deviation_bps > config.max_deviation_bps as i128 {
                state.circuit_broken = true;
                e.storage()
                    .instance()
                    .set(&StorageKey::DefiState(asset_bytes.clone()), &state);
                return Err(Error::CircuitBreakerTripped);
            }
        }

        let normalized = Self::normalize(price, decoded.decimals);

        let snapshot = PriceSnapshot { price, timestamp };
        state.history.push_back(snapshot);
        if state.history.len() > config.twap_window {
            state.history.pop_front();
        }
        state.last_price = price;

        e.storage()
            .instance()
            .set(&StorageKey::DefiState(asset_bytes.clone()), &state);

        let entry = PriceEntry {
            asset: asset_bytes.clone(),
            price,
            timestamp,
            decimals: decoded.decimals as u32,
            updated_at: now,
            updater: caller,
            normalized,
        };

        e.storage()
            .instance()
            .set(&StorageKey::Price(asset_bytes), &entry);

        Ok(())
    }

    pub fn get_price(e: Env, asset: Bytes) -> Result<PriceEntry, Error> {
        let state: DefiState = e
            .storage()
            .instance()
            .get(&StorageKey::DefiState(asset.clone()))
            .ok_or(Error::FeedNotRegistered)?;

        if state.circuit_broken {
            return Err(Error::CircuitBreakerTripped);
        }

        let entry: PriceEntry = e
            .storage()
            .instance()
            .get(&StorageKey::Price(asset.clone()))
            .ok_or(Error::AssetNotFound)?;

        let config: FeedConfig = e
            .storage()
            .instance()
            .get(&StorageKey::FeedConfig(asset))
            .ok_or(Error::FeedNotRegistered)?;

        let now = e.ledger().timestamp();
        if now > entry.timestamp && (now - entry.timestamp) > config.max_staleness {
            return Err(Error::StalePriceFeed);
        }

        Ok(entry)
    }

    pub fn get_twap(e: Env, asset: Bytes) -> Result<i128, Error> {
        let state: DefiState = e
            .storage()
            .instance()
            .get(&StorageKey::DefiState(asset.clone()))
            .ok_or(Error::FeedNotRegistered)?;

        let _config: FeedConfig = e
            .storage()
            .instance()
            .get(&StorageKey::FeedConfig(asset))
            .ok_or(Error::FeedNotRegistered)?;

        if state.history.len() < 2 {
            return Err(Error::InsufficientHistory);
        }

        let mut weighted_sum: i128 = 0;
        let mut total_time: u64 = 0;

        for i in 0..state.history.len() - 1 {
            let current = state.history.get(i).unwrap();
            let next = state.history.get(i + 1).unwrap();
            let duration = next.timestamp.saturating_sub(current.timestamp);
            weighted_sum += current.price * duration as i128;
            total_time += duration;
        }

        if total_time == 0 {
            let sum: i128 = (0..state.history.len())
                .map(|i| state.history.get(i as u32).unwrap().price)
                .sum();
            return Ok(sum / state.history.len() as i128);
        }

        Ok(weighted_sum / total_time as i128)
    }

    pub fn get_history(
        e: Env,
        asset: Bytes,
    ) -> Result<soroban_sdk::Vec<PriceSnapshot>, Error> {
        let state: DefiState = e
            .storage()
            .instance()
            .get(&StorageKey::DefiState(asset))
            .ok_or(Error::FeedNotRegistered)?;
        Ok(state.history)
    }

    pub fn get_normalized_price(e: Env, asset: Bytes) -> Result<i128, Error> {
        let entry = Self::get_price(e, asset)?;
        Ok(entry.normalized)
    }

    pub fn get_price_abi(e: Env, asset: Bytes) -> Result<Bytes, Error> {
        let entry = Self::get_price(e.clone(), asset.clone())?;
        let twap = Self::get_twap(e.clone(), asset).unwrap_or(entry.price);

        let mut asset_arr = [0u8; 32];
        let len = entry.asset.len().min(32) as usize;
        entry
            .asset
            .slice(0..len as u32)
            .copy_into_slice(&mut asset_arr[..len]);

        use alloy_sol_types::private::primitives::{I256, U256};
        let output = PriceFeedOutput {
            asset: asset_arr.into(),
            price: I256::try_from(entry.price).unwrap_or_default(),
            timestamp: U256::from(entry.timestamp),
            decimals: entry.decimals as u8,
            queried_at: U256::from(e.ledger().timestamp()),
            twap: I256::try_from(twap).unwrap_or_default(),
            normalized: I256::try_from(entry.normalized).unwrap_or_default(),
        };

        Ok(Bytes::from_slice(&e, &output.abi_encode()))
    }

    pub fn get_feed_config(e: Env, asset: Bytes) -> Result<FeedConfig, Error> {
        e.storage()
            .instance()
            .get(&StorageKey::FeedConfig(asset))
            .ok_or(Error::FeedNotRegistered)
    }

    pub fn is_fresh(e: Env, asset: Bytes) -> bool {
        Self::get_price(e, asset).is_ok()
    }

    pub fn is_circuit_broken(e: Env, asset: Bytes) -> bool {
        e.storage()
            .instance()
            .get::<StorageKey, DefiState>(&StorageKey::DefiState(asset))
            .map(|s| s.circuit_broken)
            .unwrap_or(false)
    }

    fn normalize(price: i128, decimals: u8) -> i128 {
        let target: u8 = 18;
        if decimals < target {
            let scale = 10i128.pow((target - decimals) as u32);
            price * scale
        } else if decimals > target {
            let scale = 10i128.pow((decimals - target) as u32);
            price / scale
        } else {
            price
        }
    }

    fn require_admin(e: &Env, caller: &Address) -> Result<(), Error> {
        let admin: Address = e.storage().instance().get(&StorageKey::Admin).unwrap();
        if *caller != admin {
            return Err(Error::Unauthorized);
        }
        Ok(())
    }

    fn require_updater(e: &Env, caller: &Address) -> Result<(), Error> {
        let allowed: bool = e
            .storage()
            .instance()
            .get(&StorageKey::Updater(caller.clone()))
            .unwrap_or(false);
        if !allowed {
            return Err(Error::Unauthorized);
        }
        Ok(())
    }
}

#[cfg(test)]
mod test;
