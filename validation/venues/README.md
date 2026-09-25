# Keyless venue adapters

The optional `keyless-venues` feature exposes cached-account protocol adapters through `sol_trade_sdk::venues`. This API receives public keys and immutable account data and constructs unsigned instructions. It does not instantiate SDK clients, fetch RPC state, sign, simulate or submit transactions. The existing high-level SDK APIs are unchanged and are outside this boundary.

Supported variants cover Pump.fun, PumpSwap, Raydium LaunchLab, CPMM and AMM v4, and Meteora DBC and DAMM v2. This is bounded SOL-pair support, not every variant of those protocols. Dynamic Meteora fees and other unsupported layouts continue to reject. Callers own account coherence/freshness, selection policy, balances/spending authorization, custody, signing and lifecycle recovery.

Run the public API replay with `cargo test --locked -p sol-trade-sdk --features keyless-venues --test keyless_venues`. It compares all 82 swap instructions and rejects 22 unsupported or changed dependency cases.

## Source provenance

The Meteora implementations are Rust ports of the official MIT TypeScript SDKs, not copies of the separately licensed Rust programs. See [DBC source review](dbc-integration.md), [DAMM v2 source review](damm-v2-integration.md), the adjacent exact source manifests, and the retained copyright notices. All existing quote, rounding, layout and unsupported-mode restrictions are retained from the audited implementation.

## Standalone offline integration workflow

`meteora-fixture-evidence.json` contains 41 official SDK configurations and 82 expected buy/sell instructions. `meteora-account-snapshots.json` supplies the complete synthetic account snapshots, including SPL mints/vaults and Clock, for exercising the public SDK boundary without an engine service. Expected swap bytes and ordered account privileges come from the independent MIT TypeScript oracle, not from the Rust implementation. No signing keys or network service are needed.

The application additionally exercises this SDK through real Rust services, browser sessions and loopback chain fixtures. Synthetic instruction comparisons do not execute a Solana program and do not establish funded mainnet compatibility or latency.

## Rebuild the independent oracle

Extract these pinned official repositories under `METEORA_SOURCE_ROOT` (defaults to `/private/tmp/trade-engine-meteora-review`):

- `cp-amm-sdk`: commit `37cd9e690d7b5fb6182638a21b86e0e1bf636a7e`, SDK 1.4.10.
- `dynamic-bonding-curve-sdk`: commit `a28b7239e71899eb52ff7aacac4dec90441885c4`, SDK 1.5.13.

```sh
npm ci --prefix validation/venues/oracle --ignore-scripts --no-audit --no-fund
node validation/venues/oracle/generate.mjs
```

The generator checks the committed source hashes before bundling pure quote functions and IDLs. It uses only synthetic accounts, no RPC, signing or simulation. Node dependencies and bundles are development-only and ignored. The resulting corpus must agree with the committed SHA-256 `91e8b57e9a313f8a5036dfa241a9140f9ac8c3f618419f6f63816fd2f1503302` unless a separate reviewed protocol update intentionally changes it.
