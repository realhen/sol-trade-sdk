//! Checked Raydium CPMM snapshots and keyless exact-input instruction construction.

use super::*;
use crate::{
    instruction::utils::raydium_cpmm_types::{
        amm_config_decode, pool_state_decode, PoolState, AMM_CONFIG_DISCRIMINATOR, AMM_CONFIG_SIZE,
        POOL_STATE_DISCRIMINATOR, POOL_STATE_SIZE,
    },
    trading::core::params::{RaydiumCpmmParams, TokenTransferFee},
    utils::calc::raydium_cpmm::compute_swap_amount_for_pool,
};

/// Mainnet Raydium constant-product program.
pub(super) const PROGRAM: Pubkey =
    solana_sdk::pubkey!("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C");
const CLOCK: Pubkey = solana_sdk::pubkey!("SysvarC1ock11111111111111111111111111111111");
const SYSVAR: Pubkey = solana_sdk::pubkey!("Sysvar1111111111111111111111111111111111111");
const OBSERVATION_DISC: [u8; 8] = [122, 174, 197, 53, 129, 9, 165, 132];
const SWAP_DISC: [u8; 8] = [143, 190, 90, 218, 196, 30, 51, 222];
const FEE_DENOMINATOR: u64 = 1_000_000;

fn decode(account: &Account) -> Result<PoolState> {
    ensure!(
        account.owner == PROGRAM
            && account.data.len() == 8 + POOL_STATE_SIZE
            && account.data.get(..8) == Some(POOL_STATE_DISCRIMINATOR.as_slice()),
        "Unsupported CPMM pool owner or layout"
    );
    let pool = pool_state_decode(&account.data[8..]).context("Invalid CPMM pool")?;
    ensure!(
        pool.token0_mint < pool.token1_mint
            && pool.token0_mint != SYSTEM
            && pool.creator_fee_on <= 2
            && pool.status & !7 == 0
            && pool.padding1.iter().all(|value| *value == 0)
            && pool.padding.iter().all(|value| *value == 0),
        "Unsupported CPMM pool identity or extensions"
    );
    Ok(pool)
}

/// Returns authenticated token0/token1 identities, without asserting trade readiness.
pub(super) fn identity(account: &Account) -> Result<(Pubkey, Pubkey)> {
    let decoded = decode(account)?;
    Ok((decoded.token0_mint, decoded.token1_mint))
}

/// All mutable accounts required to publish a coherent executable snapshot, including chain time.
pub(super) fn dependencies(pool: Pubkey, account: &Account) -> Result<Vec<Pubkey>> {
    let decoded = decode(account)?;
    Ok(vec![
        pool,
        decoded.amm_config,
        decoded.token0_mint,
        decoded.token1_mint,
        decoded.token0_vault,
        decoded.token1_vault,
        decoded.observation_key,
        CLOCK,
    ])
}

/// Validated cached economics; reserves exposed to the parent are oriented as mint/WSOL.
#[derive(Clone)]
pub(super) struct State {
    pub mint: Pubkey,
    pub pool: Pubkey,
    pub quote_mint: Pubkey,
    pub base_reserve: u64,
    pub quote_reserve: u64,
    pub real_base_reserve: u64,
    pub real_quote_reserve: u64,
    params: RaydiumCpmmParams,
    authority: Pubkey,
    vault0_amount: u64,
    vault1_amount: u64,
}

impl State {
    /// Validates cached ownership, PDA relationships, pool status, fees and supported token modes.
    /// No RPC calls occur; missing or incompatible dependencies reject the snapshot.
    pub(super) fn load(
        pool: Pubkey,
        mint: Pubkey,
        accounts: &HashMap<Pubkey, Option<Account>>,
    ) -> Result<Self> {
        let decoded = decode(required(accounts, &pool)?)?;
        ensure!(mint != SOL && mint != SYSTEM, "Unsupported CPMM traded mint");
        let quote_mint = if mint == decoded.token0_mint {
            decoded.token1_mint
        } else {
            ensure!(mint == decoded.token1_mint, "CPMM pool does not contain requested mint");
            decoded.token0_mint
        };
        ensure!(quote_mint == SOL, "CPMM execution currently requires a WSOL quote");
        ensure!(decoded.status & 4 == 0, "CPMM swaps are disabled");
        let clock = required(accounts, &CLOCK)?;
        ensure!(clock.owner == SYSVAR && clock.data.len() == 40, "Invalid Clock sysvar");
        let timestamp = i64::from_le_bytes(bytes(&clock.data, 32)?);
        ensure!(timestamp >= 0 && timestamp as u64 >= decoded.open_time, "CPMM pool is not open");

        let config_data = owned(accounts, decoded.amm_config, PROGRAM, AMM_CONFIG_DISCRIMINATOR)?;
        ensure!(config_data.len() == 8 + AMM_CONFIG_SIZE, "Unsupported CPMM config layout");
        let config = amm_config_decode(&config_data[8..]).context("Invalid CPMM config")?;
        let (config_address, config_bump) =
            Pubkey::find_program_address(&[b"amm_config", &config.index.to_be_bytes()], &PROGRAM);
        ensure!(
            decoded.amm_config == config_address
                && config.bump == config_bump
                && config.padding.iter().all(|value| *value == 0),
            "CPMM config PDA or reserved state mismatch"
        );
        ensure!(
            config.trade_fee_rate < FEE_DENOMINATOR
                && config.creator_fee_rate < FEE_DENOMINATOR
                && config
                    .trade_fee_rate
                    .checked_add(config.creator_fee_rate)
                    .is_some_and(|rate| rate < FEE_DENOMINATOR)
                && config
                    .protocol_fee_rate
                    .checked_add(config.fund_fee_rate)
                    .is_some_and(|rate| rate <= FEE_DENOMINATOR),
            "Invalid CPMM fee configuration"
        );
        let (authority, bump) =
            Pubkey::find_program_address(&[b"vault_and_lp_mint_auth_seed"], &PROGRAM);
        ensure!(decoded.auth_bump == bump, "CPMM authority bump mismatch");
        ensure!(
            decoded.lp_mint == pda(PROGRAM, &[b"pool_lp_mint", pool.as_ref()])
                && decoded.observation_key == pda(PROGRAM, &[b"observation", pool.as_ref()]),
            "CPMM pool account relationship mismatch"
        );
        let observation = owned(accounts, decoded.observation_key, PROGRAM, OBSERVATION_DISC)?;
        ensure!(
            observation.len() == 4075
                && observation[8] <= 1
                && u16::from_le_bytes(bytes(observation, 9)?) < 100
                && key_at(observation, 11)? == pool,
            "Invalid CPMM observation account"
        );
        let mut vault_amounts = [0; 2];
        for (index, (token_mint, token_program, vault, decimals)) in [
            (
                decoded.token0_mint,
                decoded.token0_program,
                decoded.token0_vault,
                decoded.mint0_decimals,
            ),
            (
                decoded.token1_mint,
                decoded.token1_program,
                decoded.token1_vault,
                decoded.mint1_decimals,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let mint_account = required(accounts, &token_mint)?;
            ensure!(mint_account.owner == token_program, "CPMM mint program mismatch");
            let actual_decimals = if token_mint == SOL {
                ensure!(
                    token_program == TOKEN
                        && mint_account.data.len() == 82
                        && mint_account.data[45] == 1
                        && mint_account.data[44] == 9,
                    "Invalid WSOL mint"
                );
                9
            } else {
                mint_info(mint_account)?.1
            };
            ensure!(actual_decimals == decimals, "CPMM mint decimals mismatch");
            ensure!(
                vault == pda(PROGRAM, &[b"pool_vault", pool.as_ref(), token_mint.as_ref()]),
                "CPMM vault PDA mismatch"
            );
            vault_amounts[index] =
                token_amount(accounts, vault, token_mint, authority, token_program, false)?;
        }
        let reserve0 = reserve(
            vault_amounts[0],
            decoded.protocol_fees_token0,
            decoded.fund_fees_token0,
            decoded.creator_fees_token0,
        )?;
        let reserve1 = reserve(
            vault_amounts[1],
            decoded.protocol_fees_token1,
            decoded.fund_fees_token1,
            decoded.creator_fees_token1,
        )?;
        let params = RaydiumCpmmParams {
            pool_state: pool,
            amm_config: decoded.amm_config,
            base_mint: decoded.token0_mint,
            quote_mint: decoded.token1_mint,
            base_reserve: reserve0,
            quote_reserve: reserve1,
            base_vault: decoded.token0_vault,
            quote_vault: decoded.token1_vault,
            base_token_program: decoded.token0_program,
            quote_token_program: decoded.token1_program,
            observation_state: decoded.observation_key,
            trade_fee_rate: config.trade_fee_rate,
            protocol_fee_rate: config.protocol_fee_rate,
            fund_fee_rate: config.fund_fee_rate,
            creator_fee_rate: config.creator_fee_rate,
            creator_fee_on: decoded.creator_fee_on,
            enable_creator_fee: decoded.enable_creator_fee,
            base_transfer_fee: TokenTransferFee::default(),
            quote_transfer_fee: TokenTransferFee::default(),
        };
        let (base_reserve, quote_reserve) =
            if mint == decoded.token0_mint { (reserve0, reserve1) } else { (reserve1, reserve0) };
        Ok(Self {
            mint,
            pool,
            quote_mint,
            base_reserve,
            quote_reserve,
            real_base_reserve: if mint == decoded.token0_mint {
                vault_amounts[0]
            } else {
                vault_amounts[1]
            },
            real_quote_reserve: if mint == decoded.token0_mint {
                vault_amounts[1]
            } else {
                vault_amounts[0]
            },
            params,
            authority,
            vault0_amount: vault_amounts[0],
            vault1_amount: vault_amounts[1],
        })
    }

    /// Builds only the exact-input swap; the parent owns ATA setup, WSOL funding and signing.
    /// Input/output are raw atomic units. Unsupported slippage and zero-net-output quotes fail.
    pub(super) fn instruction(
        &self,
        wallet: Pubkey,
        buy: bool,
        amount: u64,
        slippage_bps: u64,
    ) -> Result<Instruction> {
        ensure!(amount > 0 && slippage_bps < 10_000, "Invalid CPMM amount or slippage");
        let input_mint = if buy { self.quote_mint } else { self.mint };
        let output_mint = if buy { self.mint } else { self.quote_mint };
        let base_in = input_mint == self.params.base_mint;
        let (input_program, output_program, input_vault, output_vault, input_balance) = if base_in {
            (
                self.params.base_token_program,
                self.params.quote_token_program,
                self.params.base_vault,
                self.params.quote_vault,
                self.vault0_amount,
            )
        } else {
            (
                self.params.quote_token_program,
                self.params.base_token_program,
                self.params.quote_vault,
                self.params.base_vault,
                self.vault1_amount,
            )
        };
        input_balance.checked_add(amount).context("CPMM input vault would overflow")?;
        let quote = compute_swap_amount_for_pool(&self.params, base_in, amount, slippage_bps)?;
        ensure!(quote.min_amount_out > 0 && quote.amount_out > 0, "CPMM output rounds to zero");
        let mut data = Vec::with_capacity(24);
        data.extend_from_slice(&SWAP_DISC);
        data.extend_from_slice(&amount.to_le_bytes());
        data.extend_from_slice(&quote.min_amount_out.to_le_bytes());
        Ok(Instruction {
            program_id: PROGRAM,
            accounts: vec![
                AccountMeta::new_readonly(wallet, true),
                AccountMeta::new_readonly(self.authority, false),
                AccountMeta::new_readonly(self.params.amm_config, false),
                AccountMeta::new(self.pool, false),
                AccountMeta::new(ata(wallet, input_mint, input_program), false),
                AccountMeta::new(ata(wallet, output_mint, output_program), false),
                AccountMeta::new(input_vault, false),
                AccountMeta::new(output_vault, false),
                AccountMeta::new_readonly(input_program, false),
                AccountMeta::new_readonly(output_program, false),
                AccountMeta::new_readonly(input_mint, false),
                AccountMeta::new_readonly(output_mint, false),
                AccountMeta::new(self.params.observation_state, false),
            ],
            data,
        })
    }

    /// Approximate LP, protocol/fund and creator fees in basis points, rounded upward separately.
    /// On-chain rates use 1,000,000; protocol/fund rates are shares of the trading fee.
    /// Execution uses the original integer rates and direction-aware quote, never these display values.
    pub(super) fn fee_bps(&self) -> [u64; 3] {
        let p = &self.params;
        let protocol_share = p.protocol_fee_rate + p.fund_fee_rate;
        let denominator = u128::from(FEE_DENOMINATOR) * 100;
        [
            (u128::from(p.trade_fee_rate) * u128::from(FEE_DENOMINATOR - protocol_share))
                .div_ceil(denominator) as u64,
            (u128::from(p.trade_fee_rate) * u128::from(protocol_share)).div_ceil(denominator)
                as u64,
            if p.enable_creator_fee { p.creator_fee_rate.div_ceil(100) } else { 0 },
        ]
    }
}

fn reserve(amount: u64, protocol: u64, fund: u64, creator: u64) -> Result<u64> {
    let reserve = amount
        .checked_sub(protocol)
        .and_then(|v| v.checked_sub(fund))
        .and_then(|v| v.checked_sub(creator))
        .context("CPMM accrued fees exceed vault balance")?;
    ensure!(reserve > 0, "CPMM pool has no usable liquidity");
    Ok(reserve)
}
