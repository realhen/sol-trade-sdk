//! Keyless DBC SOL-pair validation and integer quotes.
//!
//! Arithmetic is ported from the MIT Meteora DBC TypeScript SDK 1.5.13;
//! provenance, exclusions and license are recorded in validation/venues/dbc-integration.md.

use super::*;
use num_bigint::BigUint;

/// Mainnet Meteora Dynamic Bonding Curve program.
pub(super) const PROGRAM: Pubkey =
    solana_sdk::pubkey!("dbcij3LWUppWqq96dh6gJWwBifmcGfLSB5D4DuSMaqN");
const CLOCK: Pubkey = solana_sdk::pubkey!("SysvarC1ock11111111111111111111111111111111");
const SYSVAR: Pubkey = solana_sdk::pubkey!("Sysvar1111111111111111111111111111111111111");
const INSTRUCTIONS: Pubkey = solana_sdk::pubkey!("Sysvar1nstructions1111111111111111111111111");
const POOL_DISC: [u8; 8] = [213, 224, 5, 209, 98, 69, 119, 92];
const CONFIG_DISC: [u8; 8] = [26, 108, 14, 123, 116, 230, 129, 43];
const SWAP_DISC: [u8; 8] = [65, 75, 63, 76, 235, 91, 91, 136];
const DEN: u64 = 1_000_000_000;
const MAX_FEE: u64 = 990_000_000;
const MIN_SQRT: u128 = 4_295_048_016;
const MAX_SQRT: u128 = 79_226_673_521_066_979_257_578_248_091;

fn pool_data(account: &Account) -> Result<&[u8]> {
    ensure!(
        account.owner == PROGRAM && account.data.len() == 424 && account.data[..8] == POOL_DISC,
        "Unsupported DBC pool owner or layout"
    );
    Ok(&account.data)
}

/// Reads base identity; load authenticates the provisional SOL quote against the config.
pub(super) fn identity(account: &Account) -> Result<(Pubkey, Pubkey)> {
    Ok((key_at(pool_data(account)?, 136)?, SOL))
}

/// Accounts required for an atomic cached snapshot; Clock controls activation and fees.
pub(super) fn dependencies(pool: Pubkey, account: &Account) -> Result<Vec<Pubkey>> {
    let d = pool_data(account)?;
    Ok(vec![pool, key_at(d, 72)?, key_at(d, 136)?, SOL, key_at(d, 168)?, key_at(d, 200)?, CLOCK])
}

#[derive(Clone)]
struct Fee {
    cliff: u64,
    first: u16,
    second: u64,
    third: u64,
    mode: u8,
    elapsed: u64,
}

/// Validated cached DBC state. No SDK client, RPC, simulation or signing key is used.
#[derive(Clone)]
pub(super) struct State {
    pub mint: Pubkey,
    pub pool: Pubkey,
    pub quote_mint: Pubkey,
    pub base_reserve: u64,
    pub quote_reserve: u64,
    pub real_base_reserve: u64,
    pub real_quote_reserve: u64,
    config: Pubkey,
    base_vault: Pubkey,
    quote_vault: Pubkey,
    base_program: Pubkey,
    sqrt: BigUint,
    start: BigUint,
    stop: BigUint,
    curve: Vec<(BigUint, BigUint)>,
    fee: Fee,
    collect_quote: bool,
    first_swap_min_fee: bool,
    creator_fee_percent: u8,
}

impl State {
    /// Authenticates exact layouts, canonical pool/vault PDAs, mint modes and chain activation.
    /// Dynamic fees reject because the pinned permissive SDK lacks the pre-swap state update.
    pub(super) fn load(
        pool: Pubkey,
        mint: Pubkey,
        accounts: &HashMap<Pubkey, Option<Account>>,
    ) -> Result<Self> {
        let d = pool_data(required(accounts, &pool)?)?;
        ensure!(key_at(d, 136)? == mint && mint != SOL && mint != SYSTEM, "DBC base mint mismatch");
        let config = key_at(d, 72)?;
        let c = owned(accounts, config, PROGRAM, CONFIG_DISC)?;
        ensure!(c.len() == 1048, "Unsupported DBC config layout");
        ensure!(key_at(c, 8)? == SOL && c[238] == 0, "DBC requires classic WSOL quote");
        let (hi, lo) = if mint > SOL { (mint, SOL) } else { (SOL, mint) };
        ensure!(
            pool == pda(PROGRAM, &[b"pool", config.as_ref(), hi.as_ref(), lo.as_ref()]),
            "DBC pool PDA mismatch"
        );
        ensure!(
            c[236] == 0
                && c[232] <= 1
                && c[233] <= 1
                && c[234] <= 1
                && c[237] <= 1
                && c[365] <= 1
                && c[245] <= 100,
            "Unsupported DBC config mode"
        );
        ensure!(
            d[304] == c[237] && d[305] == 0 && d[308] == 0 && d[370] <= 1,
            "DBC pool migrated or unsupported"
        );
        for (start, end) in [(16, 24), (371, 376), (378, 384), (400, 424)] {
            ensure!(d[start..end].iter().all(|v| *v == 0), "Unsupported DBC pool reserved fields");
        }
        // Config bytes 230..232 are documented former fee percentages, not future padding.
        for (start, end) in [(131, 136), (137, 144), (160, 168), (216, 230), (249, 256)] {
            ensure!(
                c[start..end].iter().all(|v| *v == 0),
                "Unsupported DBC config reserved fields"
            );
        }
        ensure!(
            c[136] == 0,
            "DBC dynamic fees require an independently verified pre-swap volatility update"
        );
        let clock = required(accounts, &CLOCK)?;
        ensure!(clock.owner == SYSVAR && clock.data.len() == 40, "Invalid DBC Clock sysvar");
        let timestamp = i64::from_le_bytes(bytes(&clock.data, 32)?);
        ensure!(timestamp >= 0, "Invalid DBC chain timestamp");
        let point = if c[234] == 0 { u64_at(&clock.data, 0)? } else { timestamp as u64 };
        let activation = u64_at(d, 296)?;
        let elapsed = point.checked_sub(activation).context("DBC pool not activated")?;
        let fee = Fee {
            cliff: u64_at(c, 104)?,
            second: u64_at(c, 112)?,
            third: u64_at(c, 120)?,
            first: u16::from_le_bytes(bytes(c, 128)?),
            mode: c[130],
            elapsed,
        };
        fee.validate(c[232], c[234])?;
        let base = required(accounts, &mint)?;
        let (_, decimals) = mint_info(base)?;
        ensure!(
            decimals == c[235] && base.owner == if c[237] == 0 { TOKEN } else { TOKEN_2022 },
            "DBC base mint program/decimals mismatch"
        );
        let sol = required(accounts, &SOL)?;
        ensure!(
            sol.owner == TOKEN && sol.data.len() == 82 && sol.data[44] == 9 && sol.data[45] == 1,
            "Invalid DBC WSOL mint"
        );
        let authority = pda(PROGRAM, &[b"pool_authority"]);
        let base_vault = key_at(d, 168)?;
        let quote_vault = key_at(d, 200)?;
        ensure!(
            base_vault == pda(PROGRAM, &[b"token_vault", mint.as_ref(), pool.as_ref()])
                && quote_vault == pda(PROGRAM, &[b"token_vault", SOL.as_ref(), pool.as_ref()]),
            "DBC vault PDA mismatch"
        );
        let real_base_reserve =
            token_amount(accounts, base_vault, mint, authority, base.owner, false)?;
        let real_quote_reserve = token_amount(accounts, quote_vault, SOL, authority, TOKEN, false)?;
        for vault in [base_vault, quote_vault] {
            let data = &required(accounts, &vault)?.data;
            ensure!(
                u32::from_le_bytes(bytes(data, 72)?) == 0
                    && u32::from_le_bytes(bytes(data, 129)?) == 0,
                "DBC delegated or externally closable vault unsupported"
            );
        }
        let base_reserve = u64_at(d, 232)?;
        let quote_reserve = u64_at(d, 240)?;
        ensure!(
            base_reserve > 0 && quote_reserve < u64_at(c, 264)?,
            "DBC curve completed or depleted"
        );
        for (balance, reserve, offsets) in [
            (real_base_reserve, base_reserve, [248, 264, 352]),
            (real_quote_reserve, quote_reserve, [256, 272, 360]),
        ] {
            let accounted = offsets.into_iter().try_fold(reserve, |sum, offset| {
                sum.checked_add(u64_at(d, offset)?).context("DBC reserve overflow")
            })?;
            ensure!(balance >= accounted, "DBC reserves and accrued fees exceed vault liquidity");
        }
        let sqrt = u128::from_le_bytes(bytes(d, 280)?);
        let start = u128::from_le_bytes(bytes(c, 392)?);
        let stop = u128::from_le_bytes(bytes(c, 280)?);
        ensure!(
            start >= MIN_SQRT && stop <= MAX_SQRT && start <= sqrt && sqrt < stop,
            "DBC price outside active curve"
        );
        let mut curve = Vec::new();
        let mut previous = start;
        let mut ended = false;
        for i in 0..20 {
            let price = u128::from_le_bytes(bytes(c, 408 + i * 32)?);
            let liquidity = u128::from_le_bytes(bytes(c, 424 + i * 32)?);
            if price == 0 && liquidity == 0 {
                ended = true;
                continue;
            }
            ensure!(
                !ended && price > previous && price <= MAX_SQRT && liquidity > 0,
                "Invalid DBC liquidity curve"
            );
            curve.push((BigUint::from(price), BigUint::from(liquidity)));
            previous = price;
        }
        ensure!(!curve.is_empty() && previous >= stop, "DBC curve lacks migration range");
        Ok(Self {
            mint,
            pool,
            quote_mint: SOL,
            base_reserve,
            quote_reserve,
            real_base_reserve,
            real_quote_reserve,
            config,
            base_vault,
            quote_vault,
            base_program: base.owner,
            sqrt: sqrt.into(),
            start: start.into(),
            stop: stop.into(),
            curve,
            fee,
            collect_quote: c[232] == 0,
            first_swap_min_fee: c[365] == 1,
            creator_fee_percent: c[245],
        })
    }

    /// Exact raw SOL-per-base-token spot price; decimal scaling belongs to the display layer.
    pub(super) fn spot_price_ratio(&self) -> (BigUint, BigUint) {
        (&self.sqrt * &self.sqrt, BigUint::from(1u8) << 128usize)
    }

    /// Rounded display fee shares (partner, protocol, creator), excluding amount-dependent limiter uplift.
    pub(super) fn fee_bps(&self) -> [u64; 3] {
        let total = self.fee.base(false, 0).unwrap_or(MAX_FEE) as u128;
        let protocol = total / 5;
        let creator = (total - protocol) * u128::from(self.creator_fee_percent) / 100;
        [total - protocol - creator, protocol, creator].map(|v| v.div_ceil(100_000) as u64)
    }

    /// Encodes swap2 partial-fill with a checked, fee-aware minimum; funding/signing is parent-owned.
    /// First-swap creator exemptions cannot apply: this engine never bundles pool creation.
    pub(super) fn instruction(
        &self,
        wallet: Pubkey,
        buy: bool,
        amount: u64,
        slippage_bps: u64,
    ) -> Result<Instruction> {
        ensure!(amount > 0 && slippage_bps < 10_000, "Invalid DBC amount or slippage");
        let rate = self.fee.base(buy, amount)?;
        let input_fee = buy && self.collect_quote;
        let net = if input_fee { exclude(amount, rate)? } else { amount };
        let (gross, left) = self.swap(buy, net)?;
        let consumed = net.checked_sub(left).context("DBC partial fill underflow")?;
        let actual = if left > 0 && input_fee {
            let rate = self.fee.excluded(buy, consumed)?;
            to_u64(div_up(BigUint::from(consumed) * DEN, BigUint::from(DEN - rate))?)?
        } else if left > 0 {
            consumed
        } else {
            amount
        };
        ensure!(actual > 0 && actual <= amount, "DBC partial fill exceeds input cap");
        let output = if input_fee { gross } else { exclude(gross, rate)? };
        ensure!(
            gross <= if buy { self.base_reserve } else { self.quote_reserve },
            "DBC output exceeds available curve reserves"
        );
        (if buy { self.real_quote_reserve } else { self.real_base_reserve })
            .checked_add(actual)
            .context("DBC vault input overflow")?;
        let minimum = (u128::from(output) * u128::from(10_000 - slippage_bps) / 10_000) as u64;
        ensure!(minimum > 0, "DBC output rounds to zero");
        let input = if buy { self.quote_mint } else { self.mint };
        let output_mint = if buy { self.mint } else { SOL };
        let input_program = if buy { TOKEN } else { self.base_program };
        let output_program = if buy { self.base_program } else { TOKEN };
        let mut accounts = vec![
            AccountMeta::new_readonly(pda(PROGRAM, &[b"pool_authority"]), false),
            AccountMeta::new_readonly(self.config, false),
            AccountMeta::new(self.pool, false),
            AccountMeta::new(ata(wallet, input, input_program), false),
            AccountMeta::new(ata(wallet, output_mint, output_program), false),
            AccountMeta::new(self.base_vault, false),
            AccountMeta::new(self.quote_vault, false),
            AccountMeta::new_readonly(self.mint, false),
            AccountMeta::new_readonly(SOL, false),
            AccountMeta::new_readonly(wallet, true),
            AccountMeta::new_readonly(self.base_program, false),
            AccountMeta::new_readonly(TOKEN, false),
            AccountMeta::new_readonly(PROGRAM, false),
            AccountMeta::new_readonly(pda(PROGRAM, &[b"__event_authority"]), false),
            AccountMeta::new_readonly(PROGRAM, false),
        ];
        if self.fee.limiter(buy) || self.first_swap_min_fee {
            accounts.push(AccountMeta::new_readonly(INSTRUCTIONS, false));
        }
        let mut data = SWAP_DISC.to_vec();
        data.extend(amount.to_le_bytes());
        data.extend(minimum.to_le_bytes());
        data.push(1);
        Ok(Instruction { program_id: PROGRAM, accounts, data })
    }

    fn swap(&self, buy: bool, amount: u64) -> Result<(u64, u64)> {
        let mut price = self.sqrt.clone();
        let mut left = BigUint::from(amount);
        let mut out = BigUint::from(0u8);
        if buy {
            for (limit, liquidity) in &self.curve {
                let target = limit.min(&self.stop);
                if target <= &price {
                    continue;
                }
                let max = delta_quote(&price, target, liquidity, true)?;
                if left < max {
                    let next = &price + ((&left << 128usize) / liquidity);
                    out += delta_base(&price, &next, liquidity, false)?;
                    left = BigUint::from(0u8);
                    break;
                }
                out += delta_base(&price, target, liquidity, false)?;
                left -= max;
                price = target.clone();
                if price == self.stop {
                    break;
                }
            }
        } else {
            for i in (0..self.curve.len().saturating_sub(1)).rev() {
                let target = &self.curve[i].0;
                let liquidity = &self.curve[i + 1].1;
                if target >= &price {
                    continue;
                }
                let max = delta_base(target, &price, liquidity, true)?;
                if left < max {
                    let next = next_base(&price, liquidity, &left)?;
                    out += delta_quote(&next, &price, liquidity, false)?;
                    left = BigUint::from(0u8);
                    break;
                }
                out += delta_quote(target, &price, liquidity, false)?;
                left -= max;
                price = target.clone();
            }
            if left != BigUint::from(0u8) {
                let liquidity = &self.curve[0].1;
                let next = next_base(&price, liquidity, &left)?;
                if next < self.start {
                    left = sub(left, delta_base(&self.start, &price, liquidity, true)?)?;
                    out += delta_quote(&self.start, &price, liquidity, false)?;
                } else {
                    out += delta_quote(&next, &price, liquidity, false)?;
                    left = BigUint::from(0u8);
                }
            }
        }
        Ok((to_u64(out)?, to_u64(left)?))
    }
}

fn sub(a: BigUint, b: BigUint) -> Result<BigUint> {
    ensure!(a >= b, "DBC arithmetic underflow");
    Ok(a - b)
}
fn to_u64(a: BigUint) -> Result<u64> {
    u64::try_from(a).context("DBC arithmetic exceeds u64")
}
fn div_up(n: BigUint, d: BigUint) -> Result<BigUint> {
    ensure!(d != BigUint::from(0u8), "DBC zero denominator");
    Ok((n + &d - 1u8) / d)
}
fn delta_base(lo: &BigUint, hi: &BigUint, l: &BigUint, up: bool) -> Result<BigUint> {
    ensure!(hi >= lo && lo != &BigUint::from(0u8), "DBC invalid price delta");
    let n = l * (hi - lo);
    let d = lo * hi;
    if up {
        div_up(n, d)
    } else {
        Ok(n / d)
    }
}
fn delta_quote(lo: &BigUint, hi: &BigUint, l: &BigUint, up: bool) -> Result<BigUint> {
    ensure!(hi >= lo, "DBC invalid price delta");
    let n = l * (hi - lo);
    if up {
        div_up(n, BigUint::from(1u8) << 128usize)
    } else {
        Ok(n >> 128usize)
    }
}
fn next_base(p: &BigUint, l: &BigUint, a: &BigUint) -> Result<BigUint> {
    let product = a * p;
    // Preserve the pinned SDK's explicit u128-overflow fallback and its floor rounding.
    if product.bits() > 128 {
        Ok(l / (l / p + a))
    } else {
        div_up(l * p, l + product)
    }
}
fn exclude(a: u64, rate: u64) -> Result<u64> {
    ensure!(rate <= MAX_FEE, "DBC fee exceeds cap");
    a.checked_sub((u128::from(a) * u128::from(rate)).div_ceil(u128::from(DEN)) as u64)
        .context("DBC fee exhausts amount")
}

impl Fee {
    fn validate(&self, collect: u8, activation: u8) -> Result<()> {
        ensure!(
            self.cliff > 0 && self.cliff <= MAX_FEE && self.mode <= 2,
            "Unsupported DBC base fee"
        );
        let zero = self.first == 0 && self.second == 0 && self.third == 0;
        ensure!(
            zero || self.first > 0 && self.second > 0 && self.third > 0,
            "Invalid DBC fee parameters"
        );
        if self.mode == 2 {
            ensure!(
                collect == 0
                    && self.first < 10_000
                    && self.second <= if activation == 0 { 108_000 } else { 43_200 },
                "Unsupported DBC rate limiter"
            );
        } else {
            ensure!(self.mode != 1 || self.third <= 10_000, "DBC exponential reduction invalid");
            self.scheduled(u64::from(self.first))?;
        }
        Ok(())
    }
    fn limiter(&self, buy: bool) -> bool {
        self.mode == 2 && buy && self.third > 0 && self.elapsed <= self.second
    }
    fn scheduled(&self, period: u64) -> Result<u64> {
        if self.mode == 0 {
            return self
                .cliff
                .checked_sub(period.checked_mul(self.third).context("DBC scheduler overflow")?)
                .context("DBC scheduler underflow");
        }
        let one = BigUint::from(1u8) << 64usize;
        let mut base = &one - (BigUint::from(self.third) << 64usize) / 10_000u64;
        let mut result = one.clone();
        let mut exp = period;
        while exp > 0 {
            if exp & 1 == 1 {
                result = result * &base / &one;
            }
            base = &base * &base / &one;
            exp >>= 1;
        }
        to_u64(BigUint::from(self.cliff) * result / one)
    }
    fn base(&self, buy: bool, amount: u64) -> Result<u64> {
        if self.mode == 2 {
            return if self.limiter(buy) { self.included(amount) } else { Ok(self.cliff) };
        }
        let period = self.elapsed.checked_div(self.second).unwrap_or(0).min(u64::from(self.first));
        self.scheduled(period)
    }
    fn included(&self, amount: u64) -> Result<u64> {
        if amount <= self.third {
            return Ok(self.cliff);
        }
        let c = BigUint::from(self.cliff);
        let x = BigUint::from(self.third);
        let increment = u64::from(self.first) * 100_000;
        ensure!(increment > 0, "DBC zero limiter increment");
        let max = (MAX_FEE - self.cliff) / increment;
        let diff = BigUint::from(amount) - &x;
        let a = &diff / &x;
        let b = &diff % &x;
        let n = if a < BigUint::from(max) {
            &x * (&c + &c * &a + BigUint::from(increment) * &a * (&a + 1u8) / 2u8)
                + b * (&c + BigUint::from(increment) * (&a + 1u8))
        } else {
            &x * (&c + &c * max + BigUint::from(increment) * max * (max + 1) / 2u8)
                + ((&a - max) * &x + b) * MAX_FEE
        };
        let trading = div_up(n, BigUint::from(DEN))?;
        Ok(to_u64(div_up(trading * DEN, BigUint::from(amount))?)?.min(MAX_FEE))
    }
    fn excluded(&self, buy: bool, amount: u64) -> Result<u64> {
        if !self.limiter(buy) {
            return self.base(buy, amount);
        }
        let reference = exclude(self.third, self.cliff)?;
        if amount <= reference {
            return Ok(self.cliff);
        }
        let increment = u64::from(self.first) * 100_000;
        let max = (MAX_FEE - self.cliff) / increment;
        let checked = (u128::from(max) + 1) * u128::from(self.third);
        let overflow = checked > u128::from(u64::MAX);
        let checked_in = checked.min(u128::from(u64::MAX)) as u64;
        let checked_out = exclude(checked_in, self.included(checked_in)?)?;
        if amount == checked_out {
            return self.included(checked_in);
        }
        let included = if amount < checked_out {
            let x = BigUint::from(increment);
            let x0 = BigUint::from(self.third);
            let y = BigUint::from(2 * DEN + increment - 2 * self.cliff) * &x0;
            let z = BigUint::from(amount) * DEN * 2u8 * &x0;
            let disc = sub(&y * &y, BigUint::from(4u8) * &x * z)?;
            let initial = to_u64(sub(y, disc.sqrt())? / (x * 2u8))?;
            let a_plus = initial / self.third;
            let first_out = exclude(initial, self.included(initial)?)?;
            let remaining =
                amount.checked_sub(first_out).context("DBC limiter inversion underflow")?;
            let rate = self
                .cliff
                .checked_add(increment.checked_mul(a_plus).context("DBC limiter overflow")?)
                .context("DBC limiter overflow")?;
            let denominator =
                DEN.checked_sub(rate).context("DBC limiter invalid inversion rate")?;
            BigUint::from(initial)
                + div_up(BigUint::from(remaining) * DEN, BigUint::from(denominator))?
        } else {
            ensure!(!overflow, "DBC limiter input exceeds u64");
            BigUint::from(checked_in)
                + div_up(BigUint::from(amount - checked_out) * DEN, BigUint::from(DEN - MAX_FEE))?
        };
        let fee = sub(included.clone(), BigUint::from(amount))?;
        let rate = to_u64(div_up(fee * DEN, included)?)?;
        ensure!(rate >= self.cliff && rate <= MAX_FEE, "DBC limiter inversion rate invalid");
        Ok(rate)
    }
}
