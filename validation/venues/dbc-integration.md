# Meteora DBC keyless SDK adapter

The DBC adapter supports native-SOL pairs of authenticated `VirtualPool` accounts, using cached confirmed account data and SDK-owned quote arithmetic and instruction assembly. It accepts public keys only. Custody, signing, journal admission and submission remain caller-owned. Unsigned ATA/WSOL setup is part of the shared keyless SDK builder.

## Source and license boundary

The source is the MIT `@meteora-ag/dynamic-bonding-curve-sdk` 1.5.13 at commit [`a28b7239e71899eb52ff7aacac4dec90441885c4`](https://github.com/MeteoraAg/dynamic-bonding-curve-sdk/tree/a28b7239e71899eb52ff7aacac4dec90441885c4). A fresh commit-addressed codeload archive was retrieved and the reused files compared byte-for-byte to its members. The archive SHA-256 and individual reviewed source hashes are in [dbc-source-manifest.json](dbc-source-manifest.json); the complete copyright notice is preserved in [DBC-MIT-LICENSE.txt](DBC-MIT-LICENSE.txt). GitHub's alternate repository archive endpoint produced different compressed bytes in the earlier source investigation; the unpacked source files agree. Archive hashes are endpoint-specific.

The port in `src/venues/dbc.rs` translates the following boundaries:

- IDL `VirtualPool`, `PoolState`, `PoolConfig`, fee/curve field layouts and `swap2` account privileges/discriminator.
- `helpers/pda.ts` pool, vault, pool-authority and event-authority seeds.
- `math/curve.ts` integer base/quote deltas, input next-price rounding and explicit u128-product overflow fallback.
- `math/swapQuote.ts` ascending/descending curve traversal, partial input accounting and slippage minimums.
- `math/poolFees/feeScheduler.ts`, `safeMath.ts` and `rateLimiter.ts` linear and Q64 exponential schedules, amount-dependent fee progression and excluded-fee inversion for partial fills.
- `math/feeMath.ts` fee collection orientation, ceiling fee deductions and fee splitting.
- `services/pool.ts` instruction-sysvar requirements and partial-fill parameter/account serialization. No service/client/provider constructor or RPC helper is executed at runtime.

The separately licensed Meteora Rust program source was not used. No Meteora dependency was added. The optional adapter feature uses `num-bigint` provides bounded-input arbitrary precision intermediate arithmetic; all swap quantities are checked back into u64. Curve iteration is bounded to twenty points and exponentiation to a u16 period count.

## Authenticated account contract

Pool ownership, discriminator and exactly 424 bytes must match. Its base mint/config/vaults are decoded with checked offsets. Quote identity is provisional during dependency discovery; full loading requires the config's quote mint to equal classic WSOL. Config ownership, discriminator, version zero and exactly 1048 bytes must match. Nonzero versions are rejected pending independently verified version semantics. Documented historical protocol/referral fee bytes at 230..232 may remain nonzero; only the preceding 216..230 bytes are future-reserved padding. The independent SDK corpus includes a config retaining both historical fee bytes. Delegated or externally closable vaults reject. The pool address must derive from `pool`, config, larger mint, smaller mint. Vaults derive from `token_vault`, mint, pool and must belong to the `pool_authority` PDA. Mint/token-account ownership, mint identity, decimals, initialization and supported extension checks reuse the SDK's authenticated SPL decoders.

Snapshots include Clock for slot/timestamp activation and fee scheduling. Pool migration/progress, completed quote threshold, unsupported types, malformed reserved fields, and current prices outside the configured curve reject. Curve endpoints must be positive and strictly increasing, occupied entries contiguous, and the final range must cover the migration price. Vault balances must cover recorded reserves and outstanding protocol/partner/creator fees. Donations may make balances larger; they do not increase curve liquidity. Buy and sell gross output are checked against recorded spendable curve reserves, and input vault overflow is checked before instruction assembly.

The displayed price is `sqrt_price² / 2^128` in raw SOL-per-token units. The parent applies token decimal scaling. Recorded reserve balances are not substituted for the virtual curve's price.

## Supported behavior and explicit exclusions

Both fee collection orientations, slot/timestamp activation, constant/linear/exponential base fees and existing amount-based rate limiters are supported. The SDK's inclusive limiter boundary (`current <= activation + duration`) and rate-limiter ceiling arithmetic are preserved. Display fee basis points omit the amount-dependent rate-limiter uplift; transaction quotes calculate it from the exact input.

`swap2` uses PartialFill mode with the user's original input cap and a quote-derived minimum. Curve exhaustion or migration can consume less than that cap. Input fees are recalculated through the excluded-fee path after a partial fill. The builder adds the instructions sysvar when a rate limiter is active for a buy or the config enables the first-swap minimum-fee feature. Creator first-swap exemption is never assumed: the pinned SDK documents it as a pool-initialization bundle, and this engine does not create pools. Referral accounts remain absent using Anchor's readonly program-ID placeholder.

Dynamic-enabled pools currently reject. The pinned MIT SDK's dynamic quote function computes a fee from the stored volatility accumulator but does not expose the complete Clock-dependent pre-swap volatility update. Stored-accumulator SDK agreement would not prove the next on-chain swap's fee. This implementation therefore does not substitute a maximum fee or widen the user's slippage to compensate. Dynamic fee support requires independently verified permissive update math and new differential evidence.

Market-cap base fee modes are not present for DBC swaps in the pinned SDK; they occur in migrated DAMM configuration. Unknown DBC fee modes reject. Transfer-fee/hook and other unsupported mint extensions, non-SOL routes, transfer-hook pool/config variants, exact-output orders, pool creation, migration execution, referral payouts and liquidity operations remain excluded. DBC completion requires fresh route resolution; the adapter never silently substitutes a migrated pool.

