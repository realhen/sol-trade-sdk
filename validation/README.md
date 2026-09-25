# Trading fork correctness review — 2026-09-25 UTC

Baseline: `0xfnzero/sol-trade-sdk` `0ba9ec5a652bdb351323252771fec33ea1fb2f80`, crate 5.0.5. **This is a validated compatibility patch set, not approval for unrestricted funded trading.** No funded wallet was used and no transaction was broadcast.

## Changes and evidence

1. **Mint ownership:** removed the assumption that a base58 mint ending in `pump` must use Token-2022. A supplied mint owner is now authoritative. The existing default for an unspecified owner remains Token-2022 for compatibility; callers should always provide the actual owner. Existing regression expectations that encoded the bug were corrected.
2. **Pool discovery:** removed fixed `dataSize` filters that excluded complete newer allocations (e.g. 269/270/271/301 bytes). One query now filters by program, Pool discriminator and base/quote mint. Returned data still passes owner/discriminator/layout validation. The canonical WSOL Pump pool is tried first. `pool-v2` is an auxiliary account, not a Pool state address, and is no longer fetched as state.
3. **Historical pools:** accept complete historical field boundaries and current allocations with padding; reject nonzero partial fields in the legacy reserved tail. This is consistent with the parser fork and the owner's Node parser changes.
4. **Dependency provenance:** pin the parser and streamer workspace dependencies to exact maintained fork revisions, and commit the tested Cargo.lock. No sibling checkout or floating branch is required.

The owner's Node trading fork fixes were compared with this baseline (`784ec11`, `bea80e9`). Readonly global-volume metas, writable buyback metas, one-byte CloseAccount encoding, and quote-aware cashback ATAs were already present in Rust; they were not blindly reapplied. Caller-controlled volume-tracking overrides are not ported in this patch: Rust retains its existing tracking policy, which the oracle handles explicitly.

## Independent validation

The offline oracle uses the official [Pump SDK](https://www.npmjs.com/package/@pump-fun/pump-sdk) 2.0.0, linked by the [official docs](https://github.com/pump-fun/pump-public-docs). Sixteen combinations of buy/sell, legacy/V2, SPL Token/Token-2022, and cashback compare every instruction byte and ordered account meta. The test selects a fixed amount/minimum so it validates instruction construction separately from quoting. Randomly chosen Rust buyback recipients are supplied to the official builder for this structural comparison; this does not prove those recipients remain authorized by current on-chain Global state.

`pool_discovery.rs` exercises the actual RPC client and discovery path against captured public pool data, including a 301-byte allocation. It verifies canonical-first lookup and the outgoing mint filters, and compares decoded historical pool fields with the independent oracle. The fork's Pool model does not yet expose the newest creator-fee/holder-reward tail; that economic gap is listed below.

Final offline results: 240 existing non-mainnet tests passed, two pre-existing tests remained ignored; 16 official instruction cases passed; two pool discovery/layout integration workflows passed.

An additional read-only mainnet run executed 75 existing test functions: 58 unsigned `simulateTransaction` calls succeeded across Pump/PumpSwap, Raydium AMM/CPMM/CLMM/LaunchLab, Meteora DAMM v2/DLMM, Orca, StonkFun and mixed routes. The harness uses new empty test wallets and **all-zero transaction signatures**; its funding instruction exists only inside simulation. Five cases explicitly skipped oversized transactions; other passing test functions include metadata/fixture checks rather than a distinct simulation. Initially two tests failed:

- The combined Pump buy+sell transaction exceeded Solana's 1232-byte raw limit. This remains a coverage gap and needs a valid lookup table or a different test transaction. A standalone Pump buy simulation succeeded. We did not hide this failure by changing it to a passing skip.
- Discovery of the noncanonical PUMP pool timed out. After the discovery fix, the exact live test found the pool successfully. Its subsequent combined SOL→USDC→PUMP simulation explicitly skipped an oversized transaction; discovery is verified, execution of that route is not.

These results are observations at the review date, not assurance that all future market states or launchpads work. The replay corpus and exact parser/streamer findings live in their respective `validation/README.md` files.

## Reproduction

```sh
npm ci --prefix validation --ignore-scripts
cargo +1.97.1 test --locked -p sol-trade-sdk --test official_pump_parity -- --ignored --nocapture
cargo +1.97.1 test --locked -p sol-trade-sdk --test pool_discovery
cargo +1.97.1 test --locked -p sol-trade-sdk --lib -- --skip mainnet
RUN_MAINNET_TESTS=1 PUMPFUN_MINT=<live-ungraduated-mint> cargo +1.97.1 test --locked -p sol-trade-sdk --lib mainnet -- --nocapture --test-threads=2
```

The optional live suite reads `SOLANA_RPC_URL`. Use only the reviewed `common::mainnet_sim` harness; the normal SDK simulation path has different signing behavior.

## Remaining blockers for engine adoption

- **Quoting:** `utils/calc/pumpfun.rs` uses fixed protocol/creator fee constants. The official SDK supports configurable creator fees and live schedules. `BondingCurveAccount` and the trade-side PumpSwap `Pool` omit recent fee fields, while the parser fork retains them. Do not use the convenience quotes as an authoritative minimum for arbitrary current pools. Independent state-aware quotes and explicit fixed amounts remain necessary; byte parity does not validate economic amounts.
- **Discovery scope:** canonical-first currently means the standard WSOL quote. Noncanonical fallback selects by LP supply, which is not comparable liquidity across pools. Stable/custom-quote canonical discovery and route ranking need a separate implementation and independent fixtures.
- **Existing custody/signing audit findings:** the normal simulation path still constructs a valid signed transaction; relay fanout may produce independently executable variants; asynchronous submission lifetime/receipt accounting, default constructor side effects, some TLS-bypassing/plaintext transports, unsafe performance helpers, ALT zero-entry removal, and malformed CLMM/DLMM decoder handling remain unapproved. These were reviewed against this same upstream baseline in the private engine audit. This patch does not remediate them and must not be interpreted as granting the SDK funded wallet authority.
- **Operational coverage:** mainnet simulations do not prove transaction landing, nonce exclusion, reconnect recovery, state freshness or latency under concurrent wallets. Supported instruction builders are not equivalent to complete market discovery, subscriptions, quoting and execution coverage in the engine.

Recommended engine boundary remains keyless reviewed builders/parsers, engine-owned custody/signing, and explicit protocol capability checks. Review and merge the parser PR before the streamer PR, then this PR; pins refer to their reviewed commit SHAs and remain immutable after merge.
