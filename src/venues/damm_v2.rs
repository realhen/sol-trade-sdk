//! Keyless Meteora DAMM v2 decoding, exact-input math and public-key-only swaps.
//! Integer formulas are ported from the pinned MIT cp-amm-sdk; see validation/venues/damm-v2-integration.md.

use super::*;
use num_bigint::BigUint;

/// Mainnet Meteora DAMM v2 program from the pinned official IDL.
pub(super) const PROGRAM: Pubkey =
    solana_sdk::pubkey!("cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG");
const CLOCK: Pubkey = solana_sdk::pubkey!("SysvarC1ock11111111111111111111111111111111");
const SYSVAR: Pubkey = solana_sdk::pubkey!("Sysvar1111111111111111111111111111111111111");
const INSTRUCTIONS: Pubkey = solana_sdk::pubkey!("Sysvar1nstructions1111111111111111111111111");
const POOL_DISC: [u8; 8] = [241, 154, 109, 4, 17, 177, 109, 188];
const SWAP_DISC: [u8; 8] = [65, 75, 63, 76, 235, 91, 91, 136];
const FEE_DENOMINATOR: u64 = 1_000_000_000;

fn decode(account: &Account) -> Result<&[u8]> {
    ensure!(
        account.owner == PROGRAM
            && account.data.len() == 1112
            && account.data.get(..8) == Some(POOL_DISC.as_slice()),
        "Unsupported DAMM v2 pool owner or layout"
    );
    let d = &account.data;
    ensure!(
        key_at(d, 168)? != key_at(d, 200)?
            && key_at(d, 168)? != SYSTEM
            && key_at(d, 200)? != SYSTEM,
        "Invalid DAMM v2 mint identity"
    );
    Ok(d)
}

/// Returns authenticated A/B mint identities; unsupported economics remain discoverable.
pub(super) fn identity(account: &Account) -> Result<(Pubkey, Pubkey)> {
    let d = decode(account)?;
    Ok((key_at(d, 168)?, key_at(d, 200)?))
}

/// Complete cached swap dependencies; no network reads occur while quoting or building.
pub(super) fn dependencies(pool: Pubkey, account: &Account) -> Result<Vec<Pubkey>> {
    let d = decode(account)?;
    Ok(vec![pool, key_at(d, 168)?, key_at(d, 200)?, key_at(d, 232)?, key_at(d, 264)?, CLOCK])
}

/// Validated cached state, with display reserves oriented token/WSOL.
#[derive(Clone)]
pub(super) struct State {
    pub base_reserve: u64,
    pub quote_reserve: u64,
    pub real_base_reserve: u64,
    pub real_quote_reserve: u64,
    pool: Pubkey,
    mint: Pubkey,
    mints: [Pubkey; 2],
    vaults: [Pubkey; 2],
    programs: [Pubkey; 2],
    balances: [u64; 2],
    reserves: [u64; 2],
    sqrt: BigUint,
    min: BigUint,
    max: BigUint,
    liquidity: BigUint,
    collect: u8,
    protocol_percent: u64,
    fee: BaseFee,
    current: u64,
    activation: u64,
    init_sqrt: BigUint,
}

impl State {
    /// Checks the exact layout, pool status, canonical vaults, token modes and chain activation.
    /// Dynamic fees are rejected: the pinned MIT SDK omits pre-swap volatility refresh.
    pub(super) fn load(
        pool: Pubkey,
        mint: Pubkey,
        accounts: &HashMap<Pubkey, Option<Account>>,
    ) -> Result<Self> {
        let d = decode(required(accounts, &pool)?)?;
        let mints = [key_at(d, 168)?, key_at(d, 200)?];
        ensure!(
            mint != SOL && mint != SYSTEM && mints.contains(&mint) && mints.contains(&SOL),
            "DAMM v2 execution requires a token/WSOL pair"
        );
        ensure!(
            d[480] <= 1 && d[481] == 0,
            "DAMM v2 pool is disabled or has unsupported activation"
        );
        ensure!(
            d[482] <= 1 && d[483] <= 1 && d[484] <= 2 && d[485] == 0 && d[486] <= 1 && d[696] <= 1,
            "Unsupported DAMM v2 token, collection, pool, fee or layout mode"
        );
        for range in [17..22, 40..48, 49..50, 51..54, 57..64, 416..424, 487..488, 697..728] {
            ensure!(d[range].iter().all(|b| *b == 0), "Unsupported DAMM v2 reserved state");
        }
        // padding_0 and padding_1 formerly held partner/reserve fields; historical nonzero values are valid.
        ensure!(d[56] == 0, "DAMM v2 dynamic fee refresh is unsupported");
        ensure!(d[64..152].iter().all(|b| *b == 0), "Invalid DAMM v2 disabled dynamic fee state");
        ensure!(d[728..].iter().all(|b| *b == 0), "DAMM v2 farming reward state is unsupported");
        let collect = d[484];
        let compounding_bps = u16_at(d, 54)?;
        ensure!(
            d[48] <= 100
                && d[50] <= 100
                && compounding_bps <= 10_000
                && (collect == 2 || compounding_bps == 0),
            "Invalid DAMM v2 fee split"
        );
        let clock = required(accounts, &CLOCK)?;
        ensure!(clock.owner == SYSVAR && clock.data.len() == 40, "Invalid Clock sysvar");
        let timestamp = i64::from_le_bytes(bytes(&clock.data, 32)?);
        ensure!(timestamp >= 0, "Invalid DAMM v2 chain timestamp");
        let current = if d[480] == 0 { u64_at(&clock.data, 0)? } else { timestamp as u64 };
        let activation = u64_at(d, 472)?;
        ensure!(current >= activation, "DAMM v2 pool is not activated");
        let authority = pda(PROGRAM, &[b"pool_authority"]);
        let vaults = [key_at(d, 232)?, key_at(d, 264)?];
        let mut programs = [TOKEN; 2];
        let mut balances = [0; 2];
        for i in 0..2 {
            programs[i] = if d[482 + i] == 0 { TOKEN } else { TOKEN_2022 };
            let account = required(accounts, &mints[i])?;
            ensure!(account.owner == programs[i], "DAMM v2 mint program mismatch");
            if mints[i] == SOL {
                ensure!(
                    programs[i] == TOKEN
                        && account.data.len() == 82
                        && account.data[44] == 9
                        && account.data[45] == 1,
                    "Invalid DAMM v2 WSOL mint"
                );
            } else {
                mint_info(account)?;
            }
            ensure!(
                vaults[i] == pda(PROGRAM, &[b"token_vault", mints[i].as_ref(), pool.as_ref()]),
                "DAMM v2 vault PDA mismatch"
            );
            balances[i] =
                token_amount(accounts, vaults[i], mints[i], authority, programs[i], false)?;
            let vault = required(accounts, &vaults[i])?;
            ensure!(
                u32::from_le_bytes(bytes(&vault.data, 72)?) == 0
                    && u32::from_le_bytes(bytes(&vault.data, 129)?) == 0,
                "DAMM v2 delegated or externally closable vault unsupported"
            );
        }
        let liquidity = big_at(d, 360)?;
        let min = big_at(d, 424)?;
        let max = big_at(d, 440)?;
        let sqrt = big_at(d, 456)?;
        ensure!(
            liquidity != BigUint::ZERO && sqrt != BigUint::ZERO,
            "DAMM v2 has zero liquidity or price"
        );
        let reserves = if collect == 2 {
            ensure!(
                d[696] == 1 && min == BigUint::ZERO && max == BigUint::from(u128::MAX),
                "Unsupported DAMM v2 compounding layout or bounds"
            );
            let a = u64_at(d, 680)?;
            let b = u64_at(d, 688)?;
            ensure!(a > 0 && b > 0, "DAMM v2 compounding reserves are empty");
            ensure!(
                ((BigUint::from(b) << 128usize) / BigUint::from(a)).sqrt() == sqrt,
                "DAMM v2 compounding reserve price mismatch"
            );
            [a, b]
        } else {
            ensure!(
                min >= BigUint::from(4_295_048_016u64)
                    && max <= BigUint::from(79_226_673_521_066_979_257_578_248_091u128)
                    && min < max
                    && sqrt >= min
                    && sqrt <= max,
                "Invalid DAMM v2 concentrated price bounds"
            );
            [
                to_u64(div_ceil(&liquidity * (&max - &sqrt), &sqrt * &max))?,
                to_u64(div_ceil(&liquidity * (&sqrt - &min), BigUint::from(1u8) << 128usize))?,
            ]
        };
        for i in 0..2 {
            ensure!(
                reserves[i]
                    <= balances[i]
                        .checked_sub(u64_at(d, 392 + i * 8)?)
                        .context("DAMM v2 protocol fees exceed vault")?,
                "DAMM v2 liquidity reserves exceed usable vault balance"
            );
        }
        let fee = BaseFee::load(d, collect)?;
        let init_sqrt = big_at(d, 152)?;
        if matches!(fee.mode, 3 | 4) {
            ensure!(init_sqrt != BigUint::ZERO, "DAMM v2 scheduler initial price is zero");
        }
        let token_a = mint == mints[0];
        Ok(Self {
            pool,
            mint,
            mints,
            vaults,
            programs,
            balances,
            reserves,
            sqrt,
            min,
            max,
            liquidity,
            collect,
            protocol_percent: u64::from(d[48]),
            fee,
            current,
            activation,
            init_sqrt,
            base_reserve: reserves[usize::from(!token_a)],
            quote_reserve: reserves[usize::from(token_a)],
            real_base_reserve: balances[usize::from(!token_a)],
            real_quote_reserve: balances[usize::from(token_a)],
        })
    }

    /// Raw SOL/token spot-price ratio, preserving Q64.64 price precision.
    pub(super) fn spot_price_ratio(&self) -> (BigUint, BigUint) {
        let price = &self.sqrt * &self.sqrt;
        let scale = BigUint::from(1u8) << 128usize;
        if self.mint == self.mints[0] {
            (price, scale)
        } else {
            (scale, price)
        }
    }

    fn fee_numerator(&self, amount: u64, a_to_b: bool) -> Result<u64> {
        self.fee.numerator(
            amount,
            a_to_b,
            self.current,
            self.activation,
            &self.init_sqrt,
            &self.sqrt,
        )
    }

    /// Indicative LP/protocol fees at one raw input unit; active rate limiters depend on order size.
    /// Exact integer fees are recalculated per instruction and these display values never drive trades.
    pub(super) fn fee_bps(&self) -> [u64; 3] {
        let fee = self.fee_numerator(1, false).unwrap_or(self.fee.cliff);
        [
            ((u128::from(fee) * u128::from(100 - self.protocol_percent)).div_ceil(10_000_000))
                as u64,
            ((u128::from(fee) * u128::from(self.protocol_percent)).div_ceil(10_000_000)) as u64,
            0,
        ]
    }

    /// Builds swap2 ExactIn only. The parent owns funding, ATA creation, signing and submission.
    pub(super) fn instruction(
        &self,
        wallet: Pubkey,
        buy: bool,
        amount: u64,
        slippage_bps: u64,
    ) -> Result<Instruction> {
        ensure!(amount > 0 && slippage_bps < 10_000, "Invalid DAMM v2 amount or slippage");
        let input_mint = if buy { SOL } else { self.mint };
        let input = usize::from(input_mint != self.mints[0]);
        let output = 1 - input;
        self.balances[input].checked_add(amount).context("DAMM v2 input vault would overflow")?;
        let fee = self.fee_numerator(amount, input == 0)?;
        let fees_on_input = self.collect != 0 && input == 1;
        let net_in = if fees_on_input { subtract_fee(amount, fee)? } else { amount };
        ensure!(net_in > 0, "DAMM v2 input rounds to zero after fees");
        let raw_output = if self.collect == 2 {
            to_u64(
                BigUint::from(self.reserves[output]) * BigUint::from(net_in)
                    / (BigUint::from(self.reserves[input]) + BigUint::from(net_in)),
            )?
        } else if input == 0 {
            let next = div_ceil(
                &self.liquidity * &self.sqrt,
                &self.liquidity + BigUint::from(net_in) * &self.sqrt,
            );
            ensure!(next >= self.min, "DAMM v2 input exceeds concentrated liquidity range");
            to_u64((&self.liquidity * (&self.sqrt - next)) >> 128usize)?
        } else {
            let next = &self.sqrt + (BigUint::from(net_in) << 128usize) / &self.liquidity;
            ensure!(next <= self.max, "DAMM v2 input exceeds concentrated liquidity range");
            to_u64(&self.liquidity * (&next - &self.sqrt) / (&self.sqrt * &next))?
        };
        ensure!(raw_output <= self.reserves[output], "DAMM v2 output exceeds pool reserves");
        let net_out = if fees_on_input { raw_output } else { subtract_fee(raw_output, fee)? };
        let minimum = ((u128::from(net_out) * u128::from(10_000 - slippage_bps)) / 10_000) as u64;
        ensure!(minimum > 0, "DAMM v2 output rounds to zero");
        let mut data = Vec::with_capacity(25);
        data.extend_from_slice(&SWAP_DISC);
        data.extend_from_slice(&amount.to_le_bytes());
        data.extend_from_slice(&minimum.to_le_bytes());
        data.push(0);
        let mut metas = vec![
            AccountMeta::new_readonly(pda(PROGRAM, &[b"pool_authority"]), false),
            AccountMeta::new(self.pool, false),
            AccountMeta::new(ata(wallet, self.mints[input], self.programs[input]), false),
            AccountMeta::new(ata(wallet, self.mints[output], self.programs[output]), false),
            AccountMeta::new(self.vaults[0], false),
            AccountMeta::new(self.vaults[1], false),
            AccountMeta::new_readonly(self.mints[0], false),
            AccountMeta::new_readonly(self.mints[1], false),
            AccountMeta::new_readonly(wallet, true),
            AccountMeta::new_readonly(self.programs[0], false),
            AccountMeta::new_readonly(self.programs[1], false),
            AccountMeta::new_readonly(PROGRAM, false), // Anchor optional referral sentinel.
            AccountMeta::new_readonly(pda(PROGRAM, &[b"__event_authority"]), false),
            AccountMeta::new_readonly(PROGRAM, false),
        ];
        if self.fee.limiter_applied(input == 0, self.current, self.activation) {
            metas.push(AccountMeta::new_readonly(INSTRUCTIONS, false));
        }
        Ok(Instruction { program_id: PROGRAM, accounts: metas, data })
    }
}

#[derive(Clone)]
struct BaseFee {
    mode: u8,
    cliff: u64,
    periods: u64,
    frequency: u64,
    reduction: u64,
    step: u64,
    duration: u64,
    max_fee: u64,
    protocol_maximum: u64,
}
impl BaseFee {
    fn load(d: &[u8], collect: u8) -> Result<Self> {
        let mode = d[16];
        let cliff = u64_at(d, 8)?;
        let maximum = if d[486] == 0 { 500_000_000 } else { 990_000_000 };
        ensure!(
            mode <= 4 && (100_000..=maximum).contains(&cliff),
            "Invalid DAMM v2 base fee mode or cliff"
        );
        let mut fee = Self {
            mode,
            cliff,
            periods: u64::from(u16_at(d, 22)?),
            frequency: u64_at(d, 24)?,
            reduction: u64_at(d, 32)?,
            step: 0,
            duration: 0,
            max_fee: maximum,
            protocol_maximum: maximum,
        };
        match mode {
            0 | 1 => {
                ensure!(
                    (fee.periods == 0 && fee.frequency == 0 && fee.reduction == 0)
                        || (fee.periods > 0 && fee.frequency > 0 && fee.reduction > 0),
                    "Invalid DAMM v2 time scheduler"
                );
                ensure!(
                    fee.scheduled(fee.periods)? >= 100_000,
                    "DAMM v2 terminal fee below minimum"
                );
            }
            2 => {
                fee.step = fee.periods * 100_000;
                fee.duration = u64::from(u32::from_le_bytes(bytes(d, 24)?));
                fee.max_fee = u64::from(u32::from_le_bytes(bytes(d, 28)?)) * 100_000;
                ensure!(
                    collect == 1
                        && fee.reduction > 0
                        && fee.step > 0
                        && fee.step < FEE_DENOMINATOR
                        && fee.duration > 0
                        && fee.duration <= if d[480] == 0 { 108_000 } else { 43_200 }
                        && fee.max_fee >= cliff
                        && fee.max_fee <= maximum,
                    "Invalid DAMM v2 rate limiter"
                );
                u64_at(d, 472)?
                    .checked_add(fee.duration)
                    .context("DAMM v2 limiter expiry overflow")?;
            }
            3 | 4 => {
                fee.step = u64::from(u32::from_le_bytes(bytes(d, 24)?));
                fee.duration = u64::from(u32::from_le_bytes(bytes(d, 28)?));
                ensure!(
                    fee.periods > 0 && fee.step > 0 && fee.duration > 0 && fee.reduction > 0,
                    "Invalid DAMM v2 market-cap scheduler"
                );
                ensure!(
                    fee.scheduled(fee.periods)? >= 100_000,
                    "DAMM v2 terminal fee below minimum"
                );
                u64_at(d, 472)?
                    .checked_add(fee.duration)
                    .context("DAMM v2 scheduler expiry overflow")?;
            }
            _ => unreachable!(),
        }
        Ok(fee)
    }
    fn scheduled(&self, period: u64) -> Result<u64> {
        if matches!(self.mode, 0 | 3) {
            self.cliff
                .checked_sub(
                    period.checked_mul(self.reduction).context("DAMM v2 fee reduction overflow")?,
                )
                .context("DAMM v2 linear fee underflow")
        } else {
            ensure!(self.reduction < 10_000, "Invalid DAMM v2 exponential reduction");
            if period == 0 {
                return Ok(self.cliff);
            }
            let q64 = BigUint::from(1u8) << 64usize;
            let mut base = &q64 - (BigUint::from(self.reduction) * &q64 / BigUint::from(10_000u64));
            // SDK pow uses reciprocal inversion at base >= Q64, including reduction==0.
            let invert = base >= q64;
            if invert {
                base = BigUint::from(u128::MAX) / base;
            }
            let mut result = q64;
            for bit in 0..16 {
                if period & (1u64 << bit) != 0 {
                    result = (result * &base) >> 64usize;
                }
                base = (&base * &base) >> 64usize;
            }
            if invert && result != BigUint::ZERO {
                result = BigUint::from(u128::MAX) / result;
            }
            to_u64((BigUint::from(self.cliff) * result) >> 64usize)
        }
    }
    fn limiter_applied(&self, a_to_b: bool, current: u64, activation: u64) -> bool {
        self.mode == 2 && !a_to_b && current >= activation && current - activation <= self.duration
    }
    fn numerator(
        &self,
        amount: u64,
        a_to_b: bool,
        current: u64,
        activation: u64,
        init: &BigUint,
        sqrt: &BigUint,
    ) -> Result<u64> {
        if self.mode == 2 {
            if !self.limiter_applied(a_to_b, current, activation) || amount <= self.reduction {
                return Ok(self.cliff);
            }
            let reference = self.reduction;
            let a = (amount - reference) / reference;
            let b = (amount - reference) % reference;
            let max_index = (self.max_fee - self.cliff) / self.step;
            let index = a.min(max_index);
            let c = BigUint::from(self.cliff);
            let i = BigUint::from(self.step);
            let x = BigUint::from(index);
            let first = BigUint::from(reference)
                * (&c + &c * &x + &i * &x * (&x + BigUint::from(1u8)) / BigUint::from(2u8));
            let second = if a < max_index {
                BigUint::from(b) * (&c + &i * (&x + BigUint::from(1u8)))
            } else {
                (BigUint::from(a - max_index) * BigUint::from(reference) + BigUint::from(b))
                    * BigUint::from(self.max_fee)
            };
            let trading_fee = div_ceil(first + second, BigUint::from(FEE_DENOMINATOR));
            return Ok(to_u64(div_ceil(
                trading_fee * BigUint::from(FEE_DENOMINATOR),
                BigUint::from(amount),
            ))?
            .min(self.protocol_maximum));
        }
        let period = if matches!(self.mode, 0 | 1) {
            (current - activation).checked_div(self.frequency).unwrap_or(0).min(self.periods)
        } else if current - activation > self.duration {
            self.periods
        } else if sqrt <= init {
            0
        } else {
            to_u64(
                (((sqrt - init) * BigUint::from(10_000u64) / init) / BigUint::from(self.step))
                    .min(BigUint::from(self.periods)),
            )?
        };
        self.scheduled(period)
    }
}

fn u16_at(d: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(bytes(d, offset)?))
}
fn big_at(d: &[u8], offset: usize) -> Result<BigUint> {
    Ok(BigUint::from(u128::from_le_bytes(bytes(d, offset)?)))
}
fn to_u64(value: BigUint) -> Result<u64> {
    u64::try_from(value).context("DAMM v2 arithmetic exceeds u64")
}
fn div_ceil(n: BigUint, d: BigUint) -> BigUint {
    (&n + &d - BigUint::from(1u8)) / d
}
fn subtract_fee(amount: u64, fee: u64) -> Result<u64> {
    let charge = (u128::from(amount) * u128::from(fee)).div_ceil(u128::from(FEE_DENOMINATOR));
    amount
        .checked_sub(u64::try_from(charge).context("DAMM v2 fee overflow")?)
        .context("DAMM v2 fee exceeds amount")
}
