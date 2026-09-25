//! Exercise real RPC discovery and decoding against captured public account responses.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use sol_trade_sdk::{
    common::SolanaRpcClient,
    instruction::utils::{pumpswap, pumpswap_types::pool_decode},
};
use solana_sdk::pubkey::Pubkey;
use std::{
    io::{Read, Write},
    net::TcpListener,
    str::FromStr,
    time::Duration,
};

#[test]
fn historical_pool_accounts_decode_like_official_sdk() {
    let fixture: Value =
        serde_json::from_str(include_str!("../validation/fixtures/pump-accounts.json")).unwrap();
    for e in fixture["expected"].as_array().unwrap().iter().filter(|e| e["kind"] == "pool") {
        let data = STANDARD.decode(e["data"].as_str().unwrap()).unwrap();
        let actual =
            serde_json::to_value(pool_decode(&data[8..]).expect("complete layout")).unwrap();
        for (key, value) in actual.as_object().unwrap() {
            let want = &e["fields"][key];
            assert!(
                value == want
                    || value.is_number() && want.as_str() == Some(value.to_string().as_str()),
                "{} bytes {key}: {value} != {want}",
                e["length"]
            );
        }
    }
}

#[tokio::test]
async fn mint_discovery_uses_canonical_then_mint_filter_without_size_allowlist() {
    let fixture: Value =
        serde_json::from_str(include_str!("../validation/fixtures/pump-accounts.json")).unwrap();
    let e = fixture["expected"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "pool" && e["length"] == 301)
        .unwrap();
    let data = STANDARD.decode(e["data"].as_str().unwrap()).unwrap();
    let pool = pool_decode(&data[8..]).unwrap();
    let mint = pool.base_mint;
    let address = Pubkey::from_str(e["address"].as_str().unwrap()).unwrap();
    let mut account =
        fixture["accounts"].as_array().unwrap().iter().find(|a| a["kind"] == "pool").unwrap()
            ["value"]
            .clone();
    account["data"] = json!([e["data"], "base64"]);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut seen_canonical = false;
        loop {
            let (mut socket, _) = match listener.accept() {
                Ok(v) => v,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(std::time::Instant::now() < deadline, "RPC workflow timed out");
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(e) => panic!("{e}"),
            };
            socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0u8; 4096];
            let body = loop {
                let n = socket.read(&mut chunk).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&chunk[..n]);
                if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let len: usize = headers
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + len {
                        break serde_json::from_slice::<Value>(&bytes[end + 4..end + 4 + len])
                            .unwrap();
                    }
                }
            };
            let method = body["method"].as_str().unwrap();
            let result =
                match method {
                    "getVersion" => json!({"solana-core":"4.2.2","feature-set":0}),
                    "getAccountInfo" => {
                        assert!(!seen_canonical);
                        assert_eq!(
                            body["params"][0],
                            pumpswap::get_canonical_pool_pda(&mint).to_string()
                        );
                        seen_canonical = true;
                        json!({"context":{"slot":450216901},"value":null})
                    }
                    "getProgramAccounts" => {
                        assert!(seen_canonical);
                        let filters = body["params"][1]["filters"].as_array().unwrap();
                        assert_eq!(filters.len(), 2);
                        assert!(filters.iter().all(|f| f.get("dataSize").is_none()));
                        assert!(filters.iter().any(|f| f["memcmp"]["offset"] == 43
                            && f["memcmp"]["bytes"] == mint.to_string()));
                        json!([{"pubkey":address.to_string(),"account":account}])
                    }
                    _ => panic!("unexpected RPC {method}"),
                };
            let response = json!({"jsonrpc":"2.0","id":body["id"],"result":result}).to_string();
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",response.len(),response).unwrap();
            if method == "getProgramAccounts" {
                break;
            }
        }
    });
    let (found, decoded) =
        pumpswap::find_by_mint(&SolanaRpcClient::new(endpoint), &mint).await.unwrap();
    assert_eq!(found, address);
    assert_eq!(decoded.base_mint, mint);
    server.join().unwrap();
}
