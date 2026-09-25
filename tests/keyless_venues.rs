//! Public keyless SDK workflows against independent official-SDK swap evidence.
#![cfg(feature = "keyless-venues")]

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::Value;
use sol_trade_sdk::venues::{Account, ValidatedMarket};
use solana_sdk::pubkey::Pubkey;
use std::{collections::HashMap, str::FromStr, sync::Arc};

fn key(value: &Value) -> Pubkey {
    Pubkey::from_str(value.as_str().unwrap()).unwrap()
}

fn accounts(value: &Value) -> HashMap<Pubkey, Option<Account>> {
    value
        .as_object()
        .unwrap()
        .iter()
        .map(|(address, account)| {
            let encoded =
                account["data"].as_str().unwrap_or_else(|| account["data"][0].as_str().unwrap());
            (
                Pubkey::from_str(address).unwrap(),
                Some(Account {
                    owner: key(&account["owner"]),
                    data: Arc::from(STANDARD.decode(encoded).unwrap()),
                    lamports: account["lamports"].as_u64().unwrap_or(1_000_000_000),
                }),
            )
        })
        .collect()
}

fn evidence() -> (Value, Value) {
    (
        serde_json::from_str(include_str!("../validation/venues/meteora-fixture-evidence.json"))
            .unwrap(),
        serde_json::from_str(include_str!("../validation/venues/meteora-account-snapshots.json"))
            .unwrap(),
    )
}

#[test]
fn cached_accounts_to_unsigned_swaps_match_official_sdk() {
    let (oracle, snapshots) = evidence();
    let wallet = key(&oracle["wallet"]);
    let mut swaps = 0;
    for case in oracle["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let venue = &oracle["venues"][case["venue"].as_str().unwrap()];
        let snapshot = snapshots["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == case["name"])
            .unwrap();
        let cached = accounts(&snapshot["accounts"]);
        let market = ValidatedMarket::load(key(&venue["mint"]), Some(key(&venue["pool"])), &cached)
            .unwrap_or_else(|e| panic!("{name}: {e:#}"));
        assert_eq!(market.metadata().pool, key(&venue["pool"]), "{name}");
        for trade in case["trades"].as_array().unwrap() {
            let instructions = market
                .build(
                    wallet,
                    trade["side"] == "buy",
                    trade["amount"].as_str().unwrap().parse().unwrap(),
                    100,
                )
                .unwrap_or_else(|e| panic!("{name}: {e:#}"));
            let expected = &trade["instruction"];
            let matching: Vec<_> = instructions
                .iter()
                .filter(|ix| ix.program_id == key(&expected["program"]))
                .collect();
            assert_eq!(matching.len(), 1, "{name}");
            let swap = matching[0];
            let expected_data: Vec<u8> = expected["data"]
                .as_str()
                .unwrap()
                .as_bytes()
                .chunks_exact(2)
                .map(|hex| u8::from_str_radix(std::str::from_utf8(hex).unwrap(), 16).unwrap())
                .collect();
            assert_eq!(swap.data, expected_data, "{name} {}", trade["side"]);
            let metas = expected["keys"].as_array().unwrap();
            assert_eq!(swap.accounts.len(), metas.len(), "{name}");
            for (actual, expected) in swap.accounts.iter().zip(metas) {
                assert_eq!(actual.pubkey, key(&expected["pubkey"]), "{name}");
                assert_eq!(actual.is_signer, expected["isSigner"].as_bool().unwrap(), "{name}");
                assert_eq!(actual.is_writable, expected["isWritable"].as_bool().unwrap(), "{name}");
            }
            swaps += 1;
        }
    }
    assert_eq!(swaps, 82);
}

#[test]
fn unsupported_or_changed_cached_dependencies_cannot_build_swaps() {
    let (oracle, snapshots) = evidence();
    let mut rejected = 0;
    for (name, mutations) in oracle["mutations"].as_object().unwrap() {
        let venue = &oracle["venues"][name];
        let baseline = snapshots["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == format!("{name}-linear"))
            .unwrap();
        ValidatedMarket::load(
            key(&venue["mint"]),
            Some(key(&venue["pool"])),
            &accounts(&baseline["accounts"]),
        )
        .unwrap_or_else(|e| panic!("{name} baseline must be valid: {e:#}"));
        for (kind, changes) in mutations.as_object().unwrap() {
            let mut cached = accounts(&baseline["accounts"]);
            cached.extend(accounts(changes));
            let result =
                ValidatedMarket::load(key(&venue["mint"]), Some(key(&venue["pool"])), &cached)
                    .and_then(|market| market.build(key(&oracle["wallet"]), true, 1_000_000, 100));
            assert!(result.is_err(), "{name}/{kind} unexpectedly built a swap");
            rejected += 1;
        }
    }
    assert_eq!(rejected, 22);
}
