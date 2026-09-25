/** Optional independent fixture generator. Executes only MIT TypeScript SDK source.
 * No RPC calls, SDK client constructors, signing keys or transaction submission are used.
 */
import fs from "node:fs/promises";
import path from "node:path";
import crypto from "node:crypto";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import anchor from "@coral-xyz/anchor";
import {
  PublicKey,
  Connection,
  SYSVAR_INSTRUCTIONS_PUBKEY,
} from "@solana/web3.js";
import {
  getAssociatedTokenAddressSync,
  TOKEN_PROGRAM_ID,
  NATIVE_MINT,
} from "@solana/spl-token";
import BN from "bn.js";
const { Program } = anchor;
const here = path.dirname(fileURLToPath(import.meta.url));
const sourceRoot =
  process.env.METEORA_SOURCE_ROOT || "/private/tmp/trade-engine-meteora-review";
const sourceDirs = {
  damm: path.join(sourceRoot, "cp-amm-sdk/src"),
  dbc: path.join(
    sourceRoot,
    "dynamic-bonding-curve-sdk/packages/dynamic-bonding-curve/src",
  ),
};
const commits = {
  damm: "37cd9e690d7b5fb6182638a21b86e0e1bf636a7e",
  dbc: "a28b7239e71899eb52ff7aacac4dec90441885c4",
};
const require = createRequire(import.meta.url);
const programs = {},
  quotes = {},
  manifests = {};
const expectedSources = JSON.parse(
  await fs.readFile(
    path.resolve(here, "../meteora-fixture-evidence.json"),
    "utf8",
  ),
).sources;
for (const name of ["damm", "dbc"]) {
  const dir = sourceDirs[name];
  const hashes = {};
  async function walk(dir, base = "") {
    for (const ent of (await fs.readdir(dir, { withFileTypes: true })).sort(
      (a, b) => a.name.localeCompare(b.name),
    )) {
      const rel = path.join(base, ent.name);
      if (ent.isDirectory()) await walk(path.join(dir, ent.name), rel);
      else if (/\.(ts|json)$/.test(ent.name))
        hashes[rel] = crypto
          .createHash("sha256")
          .update(await fs.readFile(path.join(dir, ent.name)))
          .digest("hex");
    }
  }
  await walk(dir);
  manifests[name] = {
    commit: commits[name],
    license: "MIT",
    sourceFiles: hashes,
  };
  if (JSON.stringify(expectedSources[name]) !== JSON.stringify(manifests[name]))
    throw Error(
      `Exact source manifest mismatch for ${name}; refusing to execute unverified SDK code`,
    );
  const entry = path.join(dir, "math/swapQuote.ts");
  const out = path.join(here, `${name}.bundle.cjs`);
  await build({
    entryPoints: [entry],
    outfile: out,
    bundle: true,
    platform: "node",
    format: "cjs",
    nodePaths: [path.join(here, "node_modules")],
    packages: "external",
    plugins: [
      {
        name: "resolve-external-deps",
        setup(b) {
          b.onResolve({ filter: /^[^./]/ }, (args) => ({
            path: require.resolve(args.path),
            external: true,
          }));
        },
      },
    ],
  });
  quotes[name] = require(out);
  const idlPath = path.join(
    dir,
    name === "damm" ? "idl/cp_amm.json" : "idl/dynamic-bonding-curve/idl.json",
  );
  const idl = JSON.parse(await fs.readFile(idlPath, "utf8"));
  programs[name] = new Program(idl, {
    connection: new Connection("http://127.0.0.1:1"),
  });
}
const pk = (n) => new PublicKey(Buffer.alloc(32, n)),
  bn = (n) => new BN(String(n));
const Q = 1n << 64n,
  L = 1_000_000_000_000n * Q;
// The RFC 8032 first test-vector public key, never a funded key.
const wallet = new PublicKey(
  Buffer.from(
    "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
    "hex",
  ),
);
const WSOL = NATIVE_MINT,
  TOKEN = TOKEN_PROGRAM_ID;
const time = 1_800_000_000,
  slot = 100;
const address = (program, ...seeds) =>
  PublicKey.findProgramAddressSync(
    seeds.map((s) => (typeof s === "string" ? Buffer.from(s) : s.toBuffer())),
    program,
  )[0];
function zero(program, type) {
  if (typeof type === "string") {
    if (type === "pubkey") return PublicKey.default;
    if (type === "bool") return false;
    if (["u64", "u128", "i64", "i128"].includes(type)) return bn(0);
    if (/^u|^i/.test(type)) return 0;
    throw Error(type);
  }
  if (type.array)
    return Array.from({ length: type.array[1] }, () =>
      zero(program, type.array[0]),
    );
  if (type.defined) {
    const def = program.idl.types.find((t) => t.name === type.defined.name);
    return Object.fromEntries(
      def.type.fields.map((f) => [f.name, zero(program, f.type)]),
    );
  }
  throw Error(JSON.stringify(type));
}
function empty(name, type) {
  return zero(programs[name], { defined: { name: type } });
}
function encoded(name, type, obj) {
  const coder = programs[name].coder.accounts;
  const { layout, discriminator } = coder.accountLayouts.get(type);
  const raw = Buffer.alloc(2048);
  const size = layout.encode(obj, raw);
  const result = Buffer.concat([
    Buffer.from(discriminator),
    raw.subarray(0, size),
  ]);
  coder.decode(type, result);
  return result.toString("base64");
}
function feeData(mode = 0) {
  const data = Buffer.alloc(32);
  data.writeBigUInt64LE(3_000_000n);
  data[8] = mode;
  if (mode <= 1) {
    data.writeUInt16LE(10, 14);
    data.writeBigUInt64LE(10n, 16);
    data.writeBigUInt64LE(mode === 0 ? 100_000n : 100n, 24);
  } else if (mode === 2) {
    data.writeUInt16LE(25, 14);
    data.writeUInt32LE(1000, 16);
    data.writeUInt32LE(1000, 20);
    data.writeBigUInt64LE(100_000n, 24);
  } else {
    data.writeUInt16LE(10, 14);
    data.writeUInt32LE(100, 16);
    data.writeUInt32LE(1000, 20);
    data.writeBigUInt64LE(mode === 3 ? 100_000n : 100n, 24);
  }
  return [...data];
}
const fixtures = {};
for (const [name, family, n] of [
  ["dbc", "dbc", 150],
  ["dbcAlternate", "dbc", 151],
  ["damm", "damm", 160],
  ["dammAlternate", "damm", 161],
]) {
  const program = programs[family].programId,
    mint = pk(152),
    config = pk(n + 20);
  const pool =
    family === "dbc"
      ? address(
          program,
          "pool",
          config,
          ...[mint, WSOL].sort((a, b) =>
            Buffer.compare(b.toBuffer(), a.toBuffer()),
          ),
        )
      : pk(n);
  const authority = address(program, "pool_authority"),
    baseVault = address(program, "token_vault", mint, pool),
    quoteVault = address(program, "token_vault", WSOL, pool);
  let state, cfg;
  if (family === "dbc") {
    state = empty(family, "virtualPool");
    cfg = empty(family, "poolConfig");
    Object.assign(state.poolState, {
      config,
      creator: wallet,
      baseMint: mint,
      baseVault,
      quoteVault,
      baseReserve: bn(1_000_000_000_000),
      quoteReserve: bn(100_000_000_000),
      sqrtPrice: bn(Q),
      activationPoint: bn(time - 25),
      hasSwap: 1,
    });
    Object.assign(cfg, {
      quoteMint: WSOL,
      feeClaimer: wallet,
      leftoverReceiver: wallet,
      collectFeeMode: 0,
      migrationOption: 1,
      activationType: 1,
      tokenDecimal: 6,
      swapBaseAmount: bn(1_000_000_000_000),
      migrationQuoteThreshold: bn(2_000_000_000_000),
      migrationBaseThreshold: bn(500_000_000_000),
      migrationSqrtPrice: bn(2n * Q),
      sqrtStartPrice: bn(Q / 2n),
    });
    cfg.poolFees.baseFee = {
      ...cfg.poolFees.baseFee,
      cliffFeeNumerator: bn(3_000_000),
      firstFactor: 10,
      secondFactor: bn(10),
      thirdFactor: bn(100_000),
      baseFeeMode: 0,
    };
    for (let i = 0; i < 3; i++)
      cfg.curve[i] = { sqrtPrice: bn(Q * (1n << BigInt(i))), liquidity: bn(L) };
  } else {
    state = empty(family, "pool");
    Object.assign(state, {
      tokenAMint: mint,
      tokenBMint: WSOL,
      tokenAVault: baseVault,
      tokenBVault: quoteVault,
      liquidity: bn(L),
      sqrtMinPrice: bn(Q / 10n),
      sqrtMaxPrice: bn(Q * 10n),
      sqrtPrice: bn(Q),
      activationPoint: bn(time - 25),
      activationType: 1,
      tokenAAmount: bn(1_000_000_000_000),
      tokenBAmount: bn(1_000_000_000_000),
      layoutVersion: 1,
      creator: wallet,
    });
    state.poolFees.baseFee.baseFeeInfo.data = feeData();
    state.poolFees.protocolFeePercent = 20;
    state.poolFees.initSqrtPrice = bn(Q);
  }
  fixtures[name] = {
    name,
    family,
    program: program.toBase58(),
    pool: pool.toBase58(),
    mint: mint.toBase58(),
    config: config.toBase58(),
    authority: authority.toBase58(),
    baseVault: baseVault.toBase58(),
    quoteVault: quoteVault.toBase58(),
    state,
    cfg,
  };
}
function clone(v) {
  if (BN.isBN(v)) return v.clone();
  if (v instanceof PublicKey) return v;
  if (Array.isArray(v)) return v.map(clone);
  if (v && typeof v === "object")
    return Object.fromEntries(
      Object.entries(v).map(([k, val]) => [k, clone(val)]),
    );
  return v;
}
const corpus = {
  schema: 1,
  scope:
    "MIT SDK decoded synthetic state and pure cached quotes; no Solana VM or funded validation",
  clock: { unixTimestamp: time, slot },
  wallet: wallet.toBase58(),
  sources: manifests,
  venues: {},
  cases: [],
};
async function addCase(
  venueName,
  label,
  mutate = () => {},
  amounts = ["1000000", "100000000"],
  currentTime = time,
  currentSlot = 100,
) {
  const base = fixtures[venueName],
    { state, cfg } = clone(base);
  mutate(state, cfg);
  const family = base.family,
    p = programs[family],
    program = p.programId;
  const pool = new PublicKey(base.pool),
    mint = new PublicKey(base.mint);
  const encodedAccounts = {
    [base.pool]: {
      owner: base.program,
      data: encoded(family, family === "dbc" ? "virtualPool" : "pool", state),
    },
  };
  if (cfg)
    encodedAccounts[base.config] = {
      owner: base.program,
      data: encoded(family, "poolConfig", cfg),
    };
  const cases = [];
  for (const [i, side] of ["buy", "sell"].entries()) {
    const amount = bn(amounts[i]);
    const point =
      (cfg ? cfg.activationType : state.activationType) === 0
        ? currentSlot
        : currentTime;
    const result =
      family === "dbc"
        ? quotes.dbc.swapQuotePartialFill(
            state,
            cfg,
            side === "sell",
            amount,
            100,
            false,
            bn(point),
            false,
          )
        : quotes.damm.swapQuoteExactInput(
            state,
            bn(point),
            amount,
            100,
            state.tokenAMint.equals(side === "buy" ? WSOL : mint),
            false,
            state.tokenAMint.equals(WSOL) ? 9 : 6,
            state.tokenBMint.equals(WSOL) ? 9 : 6,
          );
    const accounts = {
      poolAuthority: new PublicKey(base.authority),
      pool,
      inputTokenAccount: getAssociatedTokenAddressSync(
        side === "buy" ? WSOL : mint,
        wallet,
      ),
      outputTokenAccount: getAssociatedTokenAddressSync(
        side === "buy" ? mint : WSOL,
        wallet,
      ),
      payer: wallet,
      referralTokenAccount: null,
      eventAuthority: address(program, "__event_authority"),
      program,
    };
    if (family === "dbc")
      Object.assign(accounts, {
        config: new PublicKey(base.config),
        baseVault: new PublicKey(base.baseVault),
        quoteVault: new PublicKey(base.quoteVault),
        baseMint: mint,
        quoteMint: WSOL,
        tokenBaseProgram: TOKEN,
        tokenQuoteProgram: TOKEN,
      });
    else
      Object.assign(accounts, {
        tokenAVault: state.tokenAVault,
        tokenBVault: state.tokenBVault,
        tokenAMint: state.tokenAMint,
        tokenBMint: state.tokenBMint,
        tokenAProgram: TOKEN,
        tokenBProgram: TOKEN,
      });
    const feeMode =
      family === "dbc"
        ? cfg.poolFees.baseFee.baseFeeMode
        : state.poolFees.baseFee.baseFeeInfo.data[8];
    const remaining =
      (side === "buy" && feeMode === 2) ||
      (family === "dbc" && cfg.enableFirstSwapWithMinFee)
        ? [
            {
              pubkey: SYSVAR_INSTRUCTIONS_PUBKEY,
              isSigner: false,
              isWritable: false,
            },
          ]
        : [];
    const ix = await p.methods
      .swap2({
        amount0: amount,
        amount1: result.minimumAmountOut,
        swapMode: family === "dbc" ? 1 : 0,
      })
      .accountsStrict(accounts)
      .remainingAccounts(remaining)
      .instruction();
    cases.push({
      side,
      amount: amount.toString(),
      output: result.outputAmount.toString(),
      minimumOutput: result.minimumAmountOut.toString(),
      instruction: {
        program: ix.programId.toBase58(),
        data: ix.data.toString("hex"),
        keys: ix.keys.map((k) => ({
          pubkey: k.pubkey.toBase58(),
          isSigner: k.isSigner,
          isWritable: k.isWritable,
        })),
      },
    });
  }
  corpus.cases.push({
    name: `${venueName}-${label}`,
    venue: venueName,
    accounts: encodedAccounts,
    clock: currentTime,
    clockSlot: currentSlot,
    trades: cases,
  });
}
for (const name of Object.keys(fixtures)) {
  const { state, cfg, ...metadata } = fixtures[name];
  corpus.venues[name] = metadata;
  await addCase(name, "linear");
}
for (const name of ["dbc", "damm"]) {
  await addCase(name, "rounding", () => {}, ["334", "1003"]);
  await addCase(name, "quote-fees", (s, c) => {
    if (c) c.collectFeeMode = 1;
    else s.collectFeeMode = 1;
  });
  await addCase(name, "exponential", (s, c) => {
    if (c) {
      c.poolFees.baseFee.baseFeeMode = 1;
      c.poolFees.baseFee.thirdFactor = bn(100);
    } else s.poolFees.baseFee.baseFeeInfo.data = feeData(1);
  });
  await addCase(name, "limiter", (s, c) => {
    if (c) {
      c.collectFeeMode = 0;
      Object.assign(c.poolFees.baseFee, {
        baseFeeMode: 2,
        firstFactor: 25,
        secondFactor: bn(1000),
        thirdFactor: bn(100_000),
      });
    } else {
      s.collectFeeMode = 1;
      s.poolFees.baseFee.baseFeeInfo.data = feeData(2);
    }
  });
}
await addCase("dbc", "first-swap-sysvar", (s, c) => {
  c.enableFirstSwapWithMinFee = 1;
});
await addCase("damm", "compounding", (s) => {
  s.collectFeeMode = 2;
  s.poolFees.compoundingFeeBps = 5000;
  s.sqrtMinPrice = bn(0);
  s.sqrtMaxPrice = bn((1n << 128n) - 1n);
});
for (const mode of [3, 4])
  await addCase("damm", `marketcap-${mode}`, (s) => {
    s.poolFees.baseFee.baseFeeInfo.data = feeData(mode);
    s.sqrtPrice = bn(Q + Q / 20n);
  });
await addCase("dbc", "migration-partial", (s, c) => {
  c.migrationSqrtPrice = bn(Q + Q / 10_000_000n);
  c.migrationQuoteThreshold = s.poolState.quoteReserve.add(bn(100_000));
});
for (const name of ["dbc", "damm"])
  await addCase(name, "clock-advance", () => {}, undefined, time + 30);
await addCase("damm", "reversed-orientation", (s) => {
  [s.tokenAMint, s.tokenBMint] = [s.tokenBMint, s.tokenAMint];
  [s.tokenAVault, s.tokenBVault] = [s.tokenBVault, s.tokenAVault];
});
await addCase(
  "dbc",
  "cross-ranges-and-lower-bound",
  (s, c) => {
    c.curve[0].sqrtPrice = bn((Q * 3n) / 4n);
    c.curve[1].sqrtPrice = bn(Q + Q / 1000n);
    for (let i = 0; i < 3; i++) c.curve[i].liquidity = bn(100_000_000n * Q);
  },
  ["1000000", "120000000"],
);
await addCase("dbc", "limiter-partial", (s, c) => {
  c.migrationSqrtPrice = bn(Q + Q / 10_000_000n);
  c.migrationQuoteThreshold = s.poolState.quoteReserve.add(bn(100_000));
  c.collectFeeMode = 0;
  Object.assign(c.poolFees.baseFee, {
    baseFeeMode: 2,
    firstFactor: 25,
    secondFactor: bn(1000),
    thirdFactor: bn(100_000),
  });
});
for (const name of ["dbc", "damm"])
  for (const delta of [-1, 0, 1])
    await addCase(
      name,
      `limiter-reference-${delta}`,
      (s, c) => {
        if (c) {
          c.collectFeeMode = 0;
          Object.assign(c.poolFees.baseFee, {
            baseFeeMode: 2,
            firstFactor: 25,
            secondFactor: bn(1000),
            thirdFactor: bn(100_000),
          });
        } else {
          s.collectFeeMode = 1;
          s.poolFees.baseFee.baseFeeInfo.data = feeData(2);
        }
      },
      [String(100000 + delta), "1003"],
    );
for (const name of ["dbc", "damm"])
  for (const point of [time - 6, time - 5, time - 4])
    await addCase(
      name,
      `scheduler-boundary-${point}`,
      () => {},
      undefined,
      point,
    );
for (const name of ["dbc", "damm"])
  for (const point of [94, 95, 96])
    await addCase(
      name,
      `slot-boundary-${point}`,
      (s, c) => {
        if (c) {
          c.activationType = 0;
          s.poolState.activationPoint = bn(75);
        } else {
          s.activationType = 0;
          s.activationPoint = bn(75);
        }
      },
      undefined,
      time,
      point,
    );
await addCase("dbc", "legacy-fee-tombstone", (_state, config) => {
  config.padding1 = 0x1414;
});
corpus.mutations = {};
for (const [name, base] of Object.entries(fixtures)) {
  const variants = {};
  for (const kind of [
    "disabled",
    "dynamic",
    "activation",
    "vault",
    "config",
    "fee-mode",
  ]) {
    const { state, cfg } = clone(base);
    if (kind === "disabled") {
      if (cfg) state.poolState.isMigrated = 1;
      else state.poolStatus = 1;
    }
    if (kind === "dynamic") {
      if (cfg) cfg.poolFees.dynamicFee.initialized = 1;
      else state.poolFees.dynamicFee.initialized = 1;
    }
    if (kind === "activation") {
      if (cfg) state.poolState.activationPoint = bn(time + 1000);
      else state.activationPoint = bn(time + 1000);
    }
    if (kind === "vault") {
      if (cfg) state.poolState.baseVault = pk(230);
      else state.tokenAVault = pk(230);
    }
    if (kind === "config") {
      if (!cfg) continue;
      state.poolState.config = pk(231);
    }
    if (kind === "fee-mode") {
      if (cfg) cfg.poolFees.baseFee.baseFeeMode = 255;
      else state.poolFees.baseFee.baseFeeInfo.data[8] = 255;
    }
    variants[kind] = {
      [base.pool]: {
        owner: base.program,
        data: encoded(base.family, cfg ? "virtualPool" : "pool", state),
      },
    };
    if (cfg)
      variants[kind][base.config] = {
        owner: base.program,
        data: encoded(base.family, "poolConfig", cfg),
      };
  }
  corpus.mutations[name] = variants;
}
const output = path.resolve(here, "../meteora-fixture-evidence.json");
try {
  const previous = JSON.parse(await fs.readFile(output, "utf8"));
  for (const name of ["damm", "dbc"])
    if (
      JSON.stringify(previous.sources[name]) !== JSON.stringify(manifests[name])
    )
      throw Error(
        `Exact source manifest mismatch for ${name}; review the source update before changing fixtures`,
      );
} catch (error) {
  if (error.code !== "ENOENT") throw error;
}
await fs.writeFile(output, JSON.stringify(corpus, null, 2) + "\n");
console.log(
  `Generated ${corpus.cases.length} cases / ${corpus.cases.reduce((n, c) => n + c.trades.length, 0)} official SDK quotes at ${output}`,
);
