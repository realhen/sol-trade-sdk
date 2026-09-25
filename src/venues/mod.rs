//! Validated, keyless SOL-pair venue adapters over immutable cached accounts.
//!
//! This module performs no networking, signing, submission, or freshness decisions.
//! Callers supply coherent account data and public wallet addresses, retain authority
//! over market selection and freshness, and sign returned instructions themselves.
use anyhow::{anyhow, bail, ensure, Context, Result};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
mod amm_v4;
mod cpmm;
mod damm_v2;
mod dbc;
mod external;
mod launchlab;
mod parser;

/// Public program or settlement identity.
pub const PUMP: Pubkey = solana_sdk::pubkey!("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");
/// Public program or settlement identity.
pub const AMM: Pubkey = solana_sdk::pubkey!("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA");
const FEES: Pubkey = solana_sdk::pubkey!("pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ");
/// Public program or settlement identity.
pub const TOKEN: Pubkey = solana_sdk::pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
/// Public program or settlement identity.
pub const TOKEN_2022: Pubkey = solana_sdk::pubkey!("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
const ATA: Pubkey = solana_sdk::pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
/// Public program or settlement identity.
pub const SOL: Pubkey = solana_sdk::pubkey!("So11111111111111111111111111111111111111112");
/// Public program or settlement identity.
pub const SYSTEM: Pubkey = Pubkey::new_from_array([0; 32]);
const CURVE_DISC: [u8; 8] = [23, 183, 248, 55, 96, 216, 172, 96];
const POOL_DISC: [u8; 8] = [241, 154, 109, 4, 17, 177, 109, 188];
const GLOBAL_DISC: [u8; 8] = [167, 232, 232, 177, 200, 108, 114, 127];
const AMM_GLOBAL_DISC: [u8; 8] = [149, 8, 156, 202, 160, 252, 176, 217];
const FEE_DISC: [u8; 8] = [143, 52, 146, 187, 219, 123, 76, 155];

/// Public account material; absent accounts are represented by `None` in the cache.
#[derive(Clone)]
pub struct Account {
    /// Owning on-chain program.
    pub owner: Pubkey,
    /// Immutable account bytes supplied by the caller.
    pub data: Arc<[u8]>,
    /// Account native balance.
    pub lamports: u64,
}

#[derive(Clone)]
struct MarketState {
    mint: Pubkey,
    token_program: Pubkey,
    pool: Pubkey,
    creator: Pubkey,
    fee_recipient: Pubkey,
    buyback: Pubkey,
    base_reserve: u64,
    quote_reserve: u64,
    real_base: u64,
    real_quote: u64,
    fees: [u64; 3],
    sell_fees: [u64; 3],
    disabled: u8,
    amm: bool,
    extend_pool: bool,
    virtual_quote: i128,
    execution_error: Option<String>,
    external: Option<external::State>,
}

/// Reads a required existing account from the immutable cache.
pub fn required<'a>(
    accounts: &'a HashMap<Pubkey, Option<Account>>,
    key: &Pubkey,
) -> Result<&'a Account> {
    accounts.get(key).and_then(Option::as_ref).context("Required market account missing")
}
fn owned(
    accounts: &HashMap<Pubkey, Option<Account>>,
    key: Pubkey,
    program: Pubkey,
    discriminator: [u8; 8],
) -> Result<&[u8]> {
    let account = required(accounts, &key)?;
    ensure!(
        account.owner == program && account.data.get(..8) == Some(discriminator.as_slice()),
        "Market owner or discriminator mismatch"
    );
    Ok(&account.data)
}
fn bytes<const N: usize>(data: &[u8], offset: usize) -> Result<[u8; N]> {
    data.get(offset..offset + N)
        .context("Market account truncated")?
        .try_into()
        .map_err(|_| anyhow!("Invalid market field"))
}
fn u64_at(data: &[u8], offset: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(bytes(data, offset)?))
}
fn key_at(data: &[u8], offset: usize) -> Result<Pubkey> {
    Ok(Pubkey::new_from_array(bytes(data, offset)?))
}
fn optional_flag(data: &[u8], offset: usize) -> Result<bool> {
    match data.get(offset).copied().unwrap_or(0) {
        0 => Ok(false),
        1 => Ok(true),
        _ => bail!("Invalid account boolean"),
    }
}
fn optional_u64(data: &[u8], offset: usize) -> Result<u64> {
    if data.len() <= offset {
        Ok(0)
    } else {
        u64_at(data, offset)
    }
}

fn check_extensions(data: &[u8], mint: bool) -> Result<()> {
    if data.len() == if mint { 82 } else { 165 } {
        return Ok(());
    }
    ensure!(
        data.len() >= 166 && data[165] == if mint { 1 } else { 2 },
        "Invalid Token-2022 account type"
    );
    if mint {
        ensure!(data[82..165].iter().all(|b| *b == 0), "Invalid mint padding");
    }
    let mut offset = 166;
    let mut seen = HashSet::new();
    while offset < data.len() {
        if data[offset..].iter().all(|b| *b == 0) {
            break;
        }
        let kind = u16::from_le_bytes(bytes(data, offset)?);
        let len = u16::from_le_bytes(bytes(data, offset + 2)?) as usize;
        ensure!(seen.insert(kind), "Duplicate token extension");
        ensure!(
            if mint { kind == 18 || kind == 19 } else { kind == 7 && len == 0 },
            "Unsupported token transfer extension"
        );
        offset = offset.checked_add(4 + len).context("Extension length overflow")?;
        ensure!(offset <= data.len(), "Truncated token extension");
    }
    Ok(())
}

/// Validates supported SPL mint layouts and returns supply and decimals.
pub fn mint_info(account: &Account) -> Result<(u64, u8)> {
    ensure!(account.owner == TOKEN || account.owner == TOKEN_2022, "Unsupported token program");
    ensure!(account.data.len() >= 82 && account.data[45] == 1, "Mint is uninitialized");
    if account.owner == TOKEN {
        ensure!(account.data.len() == 82, "Unexpected SPL mint length");
    } else {
        check_extensions(&account.data, true)?;
    }
    let supply = u64_at(&account.data, 36)?;
    ensure!(supply > 0 && account.data[44] <= 18, "Unsupported mint supply or decimals");
    Ok((supply, account.data[44]))
}

/// Validates token identity, authority, extensions and native status before reading balance.
pub fn token_amount(
    accounts: &HashMap<Pubkey, Option<Account>>,
    address: Pubkey,
    mint: Pubkey,
    owner: Pubkey,
    program: Pubkey,
    absent_ok: bool,
) -> Result<u64> {
    let Some(account) = accounts.get(&address).context("Token account was not fetched")?.as_ref()
    else {
        ensure!(absent_ok, "Pool vault missing");
        return Ok(0);
    };
    ensure!(
        account.owner == program && account.data.len() >= 165,
        "Token account owner or length mismatch"
    );
    ensure!(
        key_at(&account.data, 0)? == mint
            && key_at(&account.data, 32)? == owner
            && account.data[108] == 1,
        "Token account mint, authority or state mismatch"
    );
    if program == TOKEN {
        ensure!(account.data.len() == 165, "Unexpected SPL token account extensions");
    } else {
        check_extensions(&account.data, false)?;
    }
    if absent_ok {
        ensure!(
            u32::from_le_bytes(bytes(&account.data, 72)?) == 0
                && u32::from_le_bytes(bytes(&account.data, 129)?) == 0,
            "Delegated or externally closable wallet account unsupported"
        );
    }
    if mint == SOL {
        ensure!(u32::from_le_bytes(bytes(&account.data, 109)?) == 1, "WSOL account is not native");
    }
    u64_at(&account.data, 64)
}

fn tier_fees(data: &[u8], cap: u128) -> Result<[u64; 3]> {
    ensure!(data.len() >= 2512, "Unsupported fee config layout");
    let decoded = parser::fees(FEES, data)?;
    ensure!(
        !decoded.fee_tiers.is_empty() && decoded.fee_tiers.len() <= 50,
        "Invalid fee tier vector"
    );
    let mut chosen = [0; 3];
    let mut previous = 0;
    for (i, tier) in decoded.fee_tiers.iter().enumerate() {
        let threshold = tier.market_cap_lamports_threshold;
        ensure!(i == 0 || threshold > previous, "Unordered fee tier thresholds");
        previous = threshold;
        let fees = [tier.fees.lp_fee_bps, tier.fees.protocol_fee_bps, tier.fees.creator_fee_bps];
        ensure!(
            fees.iter().all(|f| *f <= 10000) && fees.iter().sum::<u64>() < 10000,
            "Unsupported fee rates"
        );
        if i == 0 || cap >= threshold {
            chosen = fees;
        }
    }
    Ok(chosen)
}

fn load_pump(
    mint: Pubkey,
    selected: Option<Pubkey>,
    accounts: &HashMap<Pubkey, Option<Account>>,
) -> Result<MarketState> {
    let mint_account = required(accounts, &mint)?;
    let token_program = mint_account.owner;
    let (supply, decimals) = mint_info(mint_account)?;
    ensure!(decimals <= 18, "Unsupported mint decimals");
    let quote = required(accounts, &SOL)?;
    ensure!(
        quote.owner == TOKEN
            && quote.data.len() == 82
            && quote.data[44] == 9
            && quote.data[45] == 1,
        "Invalid WSOL mint"
    );
    let curve_key = pda(PUMP, &[b"bonding-curve", mint.as_ref()]);
    let curve = match accounts.get(&curve_key).and_then(Option::as_ref) {
        Some(_) => Some(owned(accounts, curve_key, PUMP, CURVE_DISC)?),
        None => None,
    };
    let active_curve = curve
        .map(|d| parser::curve(PUMP, d).map(|curve| !curve.complete))
        .transpose()?
        .unwrap_or(false);
    let market = if active_curve && selected.is_none_or(|pool| pool == curve_key) {
        let data = curve.unwrap();
        ensure!(
            !optional_flag(data, 81)? && !optional_flag(data, 82)? && !optional_flag(data, 124)?,
            "Mayhem, cashback or holder-reward curve unsupported"
        );
        let decoded = parser::curve(PUMP, data)?;
        let quote_mint = decoded.quote_mint;
        ensure!(quote_mint == SYSTEM || quote_mint == SOL, "Only SOL curves supported");
        ensure!(
            optional_u64(data, 115)? == 0 && !optional_flag(data, 123)?,
            "Custom creator fee curve unsupported"
        );
        let creator = decoded.creator;
        let g = owned(accounts, global(PUMP), PUMP, GLOBAL_DISC)?;
        ensure!(g.len() >= 1005, "Pump global buyback layout required");
        let base = decoded.virtual_token_reserves;
        let quote = decoded.virtual_quote_reserves;
        ensure!(base > 0 && quote > 0, "Empty curve reserves");
        let real_base = decoded.real_token_reserves;
        let curve_balance = token_amount(
            accounts,
            ata(curve_key, mint, token_program),
            mint,
            curve_key,
            token_program,
            false,
        )?;
        ensure!(
            curve_balance >= real_base
                && curve_balance <= supply
                && supply <= decoded.token_total_supply,
            "Curve supply or vault inconsistent"
        );
        let cap = (quote as u128) * (supply as u128) / (base as u128);
        let mut fees = tier_fees(owned(accounts, fee_config(PUMP), FEES, FEE_DISC)?, cap)?;
        // Pump's sell quote uses a fixed non-mayhem supply; its buy quote uses mint supply.
        let mut sell_fees = tier_fees(
            owned(accounts, fee_config(PUMP), FEES, FEE_DISC)?,
            (quote as u128) * 1_000_000_000_000_000 / (base as u128),
        )?;
        fees[0] = 0;
        sell_fees[0] = 0;
        if creator == SYSTEM {
            fees[2] = 0;
            sell_fees[2] = 0;
        }
        MarketState {
            mint,
            token_program,
            pool: curve_key,
            creator,
            fee_recipient: key_at(g, 41)?,
            buyback: key_at(g, 741)?,
            base_reserve: base,
            quote_reserve: quote,
            real_base,
            real_quote: decoded.real_quote_reserves,
            fees,
            disabled: 0,
            sell_fees,
            amm: false,
            extend_pool: false,
            virtual_quote: 0,
            execution_error: None,
            external: None,
        }
    } else {
        let pool = selected.unwrap_or_else(|| canonical_pool(mint));
        let data = owned(accounts, pool, AMM, POOL_DISC)?;
        let decoded = parser::pool(AMM, data)?;
        ensure!(
            decoded.index == 0 && decoded.creator == pda(PUMP, &[b"pool-authority", mint.as_ref()]),
            "Only canonical migrated pools supported"
        );
        ensure!(decoded.base_mint == mint && decoded.quote_mint == SOL, "Pool pair mismatch");
        ensure!(
            decoded.pool_base_token_account == ata(pool, mint, token_program)
                && decoded.pool_quote_token_account == ata(pool, SOL, TOKEN),
            "Noncanonical pool vaults"
        );
        let mayhem = optional_flag(data, 243)?;
        let cashback = optional_flag(data, 244)?;
        let holder_reward = optional_flag(data, 270)?;
        let custom_fee = optional_u64(data, 261)? != 0 || optional_flag(data, 269)?;
        let virtual_quote = decoded.virtual_quote_reserves;
        let execution_error = if virtual_quote != 0 {
            Some("Virtual quote reserve trading is not implemented".to_owned())
        } else if mayhem || cashback || holder_reward || custom_fee {
            Some("Pool fee or reward variant is not supported for execution".to_owned())
        } else if ![211, 243, 244, 245, 261, 269, 270, 300].contains(&data.len()) {
            Some("Pool account allocation is not verified for execution".to_owned())
        } else {
            None
        };
        ensure!(
            !mayhem && !cashback && !holder_reward && !custom_fee,
            "Pool fee or reward variant observation is not implemented"
        );
        let creator = decoded.coin_creator;
        let g = owned(accounts, global(AMM), AMM, AMM_GLOBAL_DISC)?;
        ensure!(g.len() >= 907, "PumpSwap global buyback layout required");
        let base = token_amount(
            accounts,
            ata(pool, mint, token_program),
            mint,
            pool,
            token_program,
            false,
        )?;
        let quote = token_amount(accounts, ata(pool, SOL, TOKEN), SOL, pool, TOKEN, false)?;
        ensure!(base > 0 && quote > 0, "Empty pool reserves");
        let effective_quote = u64::try_from(
            (quote as i128).checked_add(virtual_quote).context("Effective reserve overflow")?,
        )
        .context("Effective quote reserve out of range")?;
        ensure!(effective_quote > 0, "Empty effective quote reserves");
        let mut fees = tier_fees(
            owned(accounts, fee_config(AMM), FEES, FEE_DISC)?,
            (effective_quote as u128) * (supply as u128) / (base as u128),
        )?;
        if creator == SYSTEM {
            fees[2] = 0;
        }
        MarketState {
            mint,
            token_program,
            pool,
            creator,
            fee_recipient: key_at(g, 57)?,
            buyback: key_at(g, 643)?,
            base_reserve: base,
            quote_reserve: effective_quote,
            real_base: base,
            real_quote: quote,
            fees,
            disabled: g[56],
            sell_fees: fees,
            amm: true,
            extend_pool: data.len() < 300,
            virtual_quote,
            execution_error,
            external: None,
        }
    };
    ensure!(selected.is_none_or(|pool| pool == market.pool), "Selected market is no longer active");
    ensure!(
        market.fee_recipient != SYSTEM && market.buyback != SYSTEM,
        "Uninitialized fee recipients"
    );
    Ok(market)
}

impl ValidatedMarket {
    /// Builds exact-input unsigned swap and canonical ATA/WSOL setup instructions.
    /// The caller must verify account freshness, wallet binding and spend admission.
    pub fn build(
        &self,
        wallet: Pubkey,
        buy: bool,
        amount: u64,
        slippage_bps: u64,
    ) -> Result<Vec<Instruction>> {
        ensure!(amount > 1 && slippage_bps < 10000, "Invalid amount or slippage");
        let m = &self.state;
        ensure!(
            m.execution_error.is_none(),
            "{}",
            m.execution_error.as_deref().unwrap_or("Unsupported pool")
        );
        ensure!(m.disabled & if buy { 8 } else { 16 } == 0, "Pool trading is disabled");
        if let Some(external) = &m.external {
            let instructions = external::build(
                external,
                wallet,
                m.mint,
                m.token_program,
                buy,
                amount,
                slippage_bps,
            )?;
            return Ok(instructions);
        }
        let expected = m.output(amount, buy)?;
        let minimum = u64::try_from((expected as u128) * (10000 - slippage_bps) as u128 / 10000)?;
        ensure!(minimum > 0, "Output rounds to zero");
        let mut instructions = vec![create_ata(wallet, wallet, m.mint, m.token_program)];
        if m.amm {
            if m.extend_pool {
                instructions.push(Instruction {
                    program_id: AMM,
                    accounts: vec![
                        rw(m.pool),
                        signer(wallet),
                        ro(SYSTEM),
                        ro(pda(AMM, &[b"__event_authority"])),
                        ro(AMM),
                    ],
                    data: vec![234, 102, 194, 203, 150, 72, 62, 229],
                });
            }
            instructions.push(create_ata(wallet, wallet, SOL, TOKEN));
            if buy {
                instructions.push(solana_system_interface::instruction::transfer(
                    &wallet,
                    &ata(wallet, SOL, TOKEN),
                    amount,
                ));
                instructions.push(Instruction {
                    program_id: TOKEN,
                    accounts: vec![rw(ata(wallet, SOL, TOKEN))],
                    data: vec![17],
                });
            }
            instructions.push(m.amm_instruction(wallet, amount, minimum, buy));
            instructions.push(Instruction {
                program_id: TOKEN,
                accounts: vec![
                    rw(ata(wallet, SOL, TOKEN)),
                    rw(wallet),
                    AccountMeta::new_readonly(wallet, true),
                ],
                data: vec![9],
            });
        } else {
            instructions.push(m.curve_instruction(wallet, amount, minimum, buy));
        }
        Ok(instructions)
    }
}
impl MarketState {
    fn output(&self, amount: u64, buy: bool) -> Result<u64> {
        let base = self.base_reserve as u128;
        let quote = self.quote_reserve as u128;
        let input = amount as u128;
        let fee = |value: u128, rate: u64| -> u128 { (value * (rate as u128)).div_ceil(10000) };
        let fees = if buy { self.fees } else { self.sell_fees };
        let total: u128 = fees.iter().map(|f| *f as u128).sum();
        let output = if buy {
            let effective = if self.amm {
                let mut effective = input * 10000 / (10000 + total);
                let charged = effective + fees.iter().map(|f| fee(effective, *f)).sum::<u128>();
                if charged > input {
                    effective = effective
                        .checked_sub(charged - input)
                        .context("Input too small for fees")?;
                }
                effective.checked_sub(1).context("Input too small")?
            } else {
                (input - 1) * 10000 / (10000 + total)
            };
            let out = base.checked_mul(effective).context("Quote arithmetic overflow")?
                / (quote + effective);
            if self.amm {
                out
            } else {
                out.min(self.real_base as u128)
            }
        } else {
            let gross =
                quote.checked_mul(input).context("Quote arithmetic overflow")? / (base + input);
            let deducted = fees.iter().map(|f| fee(gross, *f)).sum::<u128>();
            ensure!(gross <= self.real_quote as u128, "Insufficient real quote reserves");
            gross.checked_sub(deducted).context("Fees exceed output")?
        };
        u64::try_from(output).context("Output overflow")
    }

    fn curve_instruction(
        &self,
        wallet: Pubkey,
        amount: u64,
        minimum: u64,
        buy: bool,
    ) -> Instruction {
        let m = self;
        let mut accounts = vec![
            ro(global(PUMP)),
            rw(m.fee_recipient),
            ro(m.mint),
            rw(m.pool),
            rw(ata(m.pool, m.mint, m.token_program)),
            rw(ata(wallet, m.mint, m.token_program)),
            signer(wallet),
            ro(SYSTEM),
        ];
        let creator = pda(PUMP, &[b"creator-vault", m.creator.as_ref()]);
        if buy {
            accounts.extend([ro(m.token_program), rw(creator)]);
        } else {
            accounts.extend([rw(creator), ro(m.token_program)]);
        }
        accounts.extend([ro(pda(PUMP, &[b"__event_authority"])), ro(PUMP)]);
        if buy {
            accounts.extend([
                ro(pda(PUMP, &[b"global_volume_accumulator"])),
                rw(pda(PUMP, &[b"user_volume_accumulator", wallet.as_ref()])),
            ]);
        }
        accounts.extend([
            ro(fee_config(PUMP)),
            ro(FEES),
            ro(pda(PUMP, &[b"bonding-curve-v2", m.mint.as_ref()])),
            rw(m.buyback),
        ]);
        Instruction {
            program_id: PUMP,
            accounts,
            data: swap_data(
                if buy {
                    [56, 252, 116, 8, 158, 223, 205, 95]
                } else {
                    [51, 230, 133, 164, 1, 127, 131, 173]
                },
                amount,
                minimum,
                buy,
            ),
        }
    }

    fn amm_instruction(&self, wallet: Pubkey, amount: u64, minimum: u64, buy: bool) -> Instruction {
        let m = self;
        let creator = pda(AMM, &[b"creator_vault", m.creator.as_ref()]);
        let mut accounts = vec![
            rw(m.pool),
            signer(wallet),
            ro(global(AMM)),
            ro(m.mint),
            ro(SOL),
            rw(ata(wallet, m.mint, m.token_program)),
            rw(ata(wallet, SOL, TOKEN)),
            rw(ata(m.pool, m.mint, m.token_program)),
            rw(ata(m.pool, SOL, TOKEN)),
            ro(m.fee_recipient),
            rw(ata(m.fee_recipient, SOL, TOKEN)),
            ro(m.token_program),
            ro(TOKEN),
            ro(SYSTEM),
            ro(ATA),
            ro(pda(AMM, &[b"__event_authority"])),
            ro(AMM),
            rw(ata(creator, SOL, TOKEN)),
            ro(creator),
        ];
        if buy {
            accounts.extend([
                ro(pda(AMM, &[b"global_volume_accumulator"])),
                rw(pda(AMM, &[b"user_volume_accumulator", wallet.as_ref()])),
            ]);
        }
        accounts.extend([ro(fee_config(AMM)), ro(FEES)]);
        if m.creator != SYSTEM {
            accounts.push(ro(pda(AMM, &[b"pool-v2", m.mint.as_ref()])));
        }
        accounts.extend([ro(m.buyback), rw(ata(m.buyback, SOL, TOKEN))]);
        Instruction {
            program_id: AMM,
            accounts,
            data: swap_data(
                if buy {
                    [198, 46, 21, 82, 180, 217, 232, 112]
                } else {
                    [51, 230, 133, 164, 1, 127, 131, 173]
                },
                amount,
                minimum,
                buy,
            ),
        }
    }
}

fn swap_data(discriminator: [u8; 8], amount: u64, minimum: u64, buy: bool) -> Vec<u8> {
    let mut data = Vec::with_capacity(25);
    data.extend(discriminator);
    data.extend(amount.to_le_bytes());
    data.extend(minimum.to_le_bytes());
    if buy {
        data.push(1);
    }
    data
}
fn pda(program: Pubkey, seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &program).0
}
fn global(program: Pubkey) -> Pubkey {
    pda(program, &[if program == PUMP { b"global" } else { b"global_config" }])
}
fn fee_config(program: Pubkey) -> Pubkey {
    pda(FEES, &[b"fee_config", program.as_ref()])
}
/// Derives a canonical associated token address without accessing its account.
pub fn ata(owner: Pubkey, mint: Pubkey, program: Pubkey) -> Pubkey {
    pda(ATA, &[owner.as_ref(), program.as_ref(), mint.as_ref()])
}
/// Derives the canonical index-zero Pump migration pool.
pub fn canonical_pool(mint: Pubkey) -> Pubkey {
    pda(
        AMM,
        &[
            b"pool",
            &0_u16.to_le_bytes(),
            pda(PUMP, &[b"pool-authority", mint.as_ref()]).as_ref(),
            mint.as_ref(),
            SOL.as_ref(),
        ],
    )
}
fn ro(key: Pubkey) -> AccountMeta {
    AccountMeta::new_readonly(key, false)
}
fn rw(key: Pubkey) -> AccountMeta {
    AccountMeta::new(key, false)
}
fn signer(key: Pubkey) -> AccountMeta {
    AccountMeta::new(key, true)
}
fn create_ata(payer: Pubkey, owner: Pubkey, mint: Pubkey, program: Pubkey) -> Instruction {
    Instruction {
        program_id: ATA,
        accounts: vec![
            signer(payer),
            rw(ata(owner, mint, program)),
            ro(owner),
            ro(mint),
            ro(SYSTEM),
            ro(program),
        ],
        data: vec![1],
    }
}

/// Supported validated market families; callers choose their own selection policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Venue {
    /// Pump bonding curve.
    Pump,
    /// Canonical PumpSwap migrated pool.
    PumpSwap,
    /// Raydium constant-product pool.
    Cpmm,
    /// Raydium AMM v4 orderbook-idle pool.
    AmmV4,
    /// Raydium LaunchLab constant-product curve.
    LaunchLab,
    /// Meteora Dynamic Bonding Curve.
    Dbc,
    /// Meteora Dynamic AMM v2.
    DammV2,
}

impl Venue {
    /// Human-readable protocol name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Pump => "Pump.fun",
            Self::PumpSwap => "PumpSwap",
            Self::Cpmm => "Raydium CPMM",
            Self::AmmV4 => "Raydium AMM v4",
            Self::LaunchLab => "LaunchLab",
            Self::Dbc => "Meteora DBC",
            Self::DammV2 => "Meteora DAMM v2",
        }
    }
}

/// Immutable validated venue state. Construct only through [`ValidatedMarket::load`].
#[derive(Clone)]
pub struct ValidatedMarket {
    state: MarketState,
    metadata: MarketMetadata,
}

/// Display and admission metadata derived from fully validated account relationships.
#[derive(Clone)]
pub struct MarketMetadata {
    /// Traded token mint.
    pub mint: Pubkey,
    /// Validated token program.
    pub token_program: Pubkey,
    /// Pool or curve address.
    pub pool: Pubkey,
    /// Supported venue family.
    pub venue: Venue,
    /// Raw mint supply.
    pub supply: u64,
    /// Token decimal precision.
    pub decimals: u8,
    /// Effective token reserve or curve depth.
    pub base_reserve: u64,
    /// Effective SOL reserve or curve depth.
    pub quote_reserve: u64,
    /// Available raw token reserve.
    pub real_base: u64,
    /// Available raw SOL reserve.
    pub real_quote: u64,
    /// Buy fee display rates in basis points; use the builder for exact fee arithmetic.
    pub fees: [u64; 3],
    /// Sell fee display rates in basis points.
    pub sell_fees: [u64; 3],
    /// Total buy fee display rate, preserving the venue-specific rounding above.
    pub total_fee_bps: u64,
    /// Signed virtual SOL reserve adjustment.
    pub virtual_quote: i128,
    /// Reason observation is available but execution is unsupported.
    pub execution_error: Option<String>,
    /// Whether setup closes canonical WSOL and therefore requires an empty balance.
    pub requires_empty_wsol: bool,
    /// Pool accounting classification for display.
    pub account_model: &'static str,
    /// Decoder provenance identifier.
    pub parser: &'static str,
    /// Exact SOL-per-whole-token spot ratio, not a trade quote.
    pub spot_price_sol: (num_bigint::BigUint, num_bigint::BigUint),
    /// Exact supply-based SOL market-cap ratio, not a trade quote.
    pub market_cap_sol: (num_bigint::BigUint, num_bigint::BigUint),
}

impl ValidatedMarket {
    /// Validates a selected venue from a complete immutable account cache.
    /// `None` follows active Pump provenance only; broader discovery/selection belongs to callers.
    pub fn load(
        mint: Pubkey,
        selected: Option<Pubkey>,
        accounts: &HashMap<Pubkey, Option<Account>>,
    ) -> Result<Self> {
        let state = if let Some(pool) = selected.filter(|pool| {
            accounts.get(pool).and_then(Option::as_ref).is_some_and(|a| external::supports(a.owner))
        }) {
            external::load(mint, pool, accounts)?
        } else {
            load_pump(mint, selected, accounts)?
        };
        let (supply, decimals) = mint_info(required(accounts, &mint)?)?;
        let venue = match &state.external {
            Some(external::State::Cpmm(_)) => Venue::Cpmm,
            Some(external::State::AmmV4(_)) => Venue::AmmV4,
            Some(external::State::LaunchLab(_)) => Venue::LaunchLab,
            Some(external::State::Dbc(_)) => Venue::Dbc,
            Some(external::State::DammV2(_)) => Venue::DammV2,
            None if state.amm => Venue::PumpSwap,
            None => Venue::Pump,
        };
        let (quote, base) = state
            .external
            .as_ref()
            .and_then(external::spot_price_ratio)
            .unwrap_or_else(|| (state.quote_reserve.into(), state.base_reserve.into()));
        let metadata = MarketMetadata {
            mint,
            token_program: state.token_program,
            pool: state.pool,
            venue,
            supply,
            decimals,
            base_reserve: state.base_reserve,
            quote_reserve: state.quote_reserve,
            real_base: state.real_base,
            real_quote: state.real_quote,
            fees: state.fees,
            sell_fees: state.sell_fees,
            total_fee_bps: state.fees.iter().sum(),
            virtual_quote: state.virtual_quote,
            execution_error: state.execution_error.clone(),
            requires_empty_wsol: state.amm,
            account_model: if matches!(venue, Venue::LaunchLab | Venue::Dbc) {
                "bonding_curve"
            } else if state.virtual_quote == 0 {
                "standard"
            } else {
                "virtual_quote_reserves"
            },
            parser: state.external.as_ref().map(external::parser).unwrap_or(parser::PARSER),
            spot_price_sol: (
                &quote * num_bigint::BigUint::from(10u128.pow(decimals as u32)),
                &base * num_bigint::BigUint::from(1_000_000_000u64),
            ),
            market_cap_sol: (
                &quote * num_bigint::BigUint::from(supply),
                &base * num_bigint::BigUint::from(1_000_000_000u64),
            ),
        };
        Ok(Self { state, metadata })
    }

    /// Returns read-only metadata; cloning it cannot modify validated instruction state.
    pub fn metadata(&self) -> &MarketMetadata {
        &self.metadata
    }
}

/// Public identity for the bounded Pump account parser used by this module.
pub const PARSER: &str = parser::PARSER;

/// Whether a program is a supported non-Pump adapter.
pub fn supports_external(program: Pubkey) -> bool {
    external::supports(program)
}

/// Reads the traded pair from supported non-Pump pool bytes; readiness requires full validation.
pub fn external_identity(account: &Account) -> Result<(Pubkey, Pubkey)> {
    external::identity(account)
}

/// Returns required non-Pump accounts without fetching them.
pub fn external_dependencies(pool: Pubkey, account: &Account) -> Result<Vec<Pubkey>> {
    external::dependencies(pool, account)
}

/// Canonical Pump bonding curve address.
pub fn bonding_curve(mint: Pubkey) -> Pubkey {
    pda(PUMP, &[b"bonding-curve", mint.as_ref()])
}

/// Canonical LaunchLab token/SOL curve candidate; existence does not establish readiness.
pub fn launchlab_pool(mint: Pubkey) -> Pubkey {
    pda(launchlab::PROGRAM, &[b"pool", mint.as_ref(), SOL.as_ref()])
}

/// Complete Pump curve and migration-pool dependency recipe, without wallet accounts.
pub fn pump_dependencies(mint: Pubkey, pool: Pubkey, token_program: Pubkey) -> Vec<Pubkey> {
    let curve = bonding_curve(mint);
    vec![
        mint,
        SOL,
        curve,
        pool,
        ata(curve, mint, token_program),
        ata(pool, mint, token_program),
        ata(pool, SOL, TOKEN),
        global(PUMP),
        global(AMM),
        fee_config(PUMP),
        fee_config(AMM),
    ]
}

/// Reads a PumpSwap input address, enforcing canonical native-SOL settlement identity.
pub fn pump_pool_mint(pool: Pubkey, account: &Account) -> Result<Pubkey> {
    ensure!(
        account.owner == AMM && account.data.get(..8) == Some(POOL_DISC.as_slice()),
        "Market owner or discriminator mismatch"
    );
    let decoded = parser::pool(AMM, &account.data)?;
    ensure!(decoded.quote_mint == SOL, "Non-SOL settlement is not implemented");
    ensure!(
        pool == canonical_pool(decoded.base_mint),
        "Noncanonical pool unsupported; PumpSwap flat-fee integration is not implemented"
    );
    Ok(decoded.base_mint)
}

/// Reads active Pump provenance; absent curve accounts mean no active curve.
pub fn active_pump_curve(
    mint: Pubkey,
    accounts: &HashMap<Pubkey, Option<Account>>,
) -> Result<bool> {
    let curve = bonding_curve(mint);
    match accounts.get(&curve).and_then(Option::as_ref) {
        Some(_) => {
            let data = owned(accounts, curve, PUMP, CURVE_DISC)?;
            ensure!(data.len() >= 49, "Truncated bonding curve");
            Ok(!parser::curve(PUMP, data)?.complete)
        }
        None => Ok(false),
    }
}

/// Validates canonical Pump migration provenance, without deciding selection policy.
pub fn validate_pump_migration(
    mint: Pubkey,
    pool: Pubkey,
    accounts: &HashMap<Pubkey, Option<Account>>,
) -> Result<()> {
    let decoded = parser::pool(AMM, owned(accounts, pool, AMM, POOL_DISC)?)?;
    ensure!(
        decoded.index == 0
            && decoded.creator == pda(PUMP, &[b"pool-authority", mint.as_ref()])
            && decoded.base_mint == mint
            && decoded.quote_mint == SOL,
        "Canonical pool identity mismatch"
    );
    Ok(())
}

/// Pure program-account discovery filter; the caller supplies RPC and limits/ranks results.
#[derive(Clone, Copy)]
pub struct DiscoveryFilter {
    /// Program whose accounts to enumerate.
    pub program: Pubkey,
    /// Exact supported account size.
    pub data_size: usize,
    /// Byte position of a mint key for an RPC memcmp filter.
    pub mint_offset: usize,
}

/// Non-Pump SOL-pair candidate filters. Duplicate single-sided mint offsets are omitted.
pub fn discovery_filters() -> Vec<DiscoveryFilter> {
    [
        (cpmm::PROGRAM, &[168, 200][..], 637),
        (amm_v4::PROGRAM, &[400, 432][..], 752),
        (dbc::PROGRAM, &[136][..], 424),
        (damm_v2::PROGRAM, &[168, 200][..], 1112),
    ]
    .into_iter()
    .flat_map(|(program, offsets, data_size)| {
        offsets.iter().map(move |mint_offset| DiscoveryFilter {
            program,
            data_size,
            mint_offset: *mint_offset,
        })
    })
    .collect()
}

/// Unverified CLMM preview encoding, separate from validated executable venues.
pub mod clmm_preview;
