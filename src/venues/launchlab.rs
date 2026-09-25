//! Cached constant-product LaunchLab quotes and public-key-only instruction encoding.

use super::*;
use crate::{
    instruction::utils::bonk_types::{pool_state_decode, PoolState, POOL_STATE_DISCRIMINATOR},
    trading::core::params::BonkParams,
    utils::calc::bonk::{get_buy_quote, get_sell_min_amount_out},
};

pub(super) const PROGRAM: Pubkey =
    solana_sdk::pubkey!("LanMV9sAd7wArD4vJFi2qDdfnVhFxYSUg6eADduJ3uj");
const GLOBAL_CONFIG_DISC: [u8; 8] = [149, 8, 156, 202, 160, 252, 176, 217];
const PLATFORM_CONFIG_DISC: [u8; 8] = [160, 78, 128, 0, 248, 83, 230, 160];
const BUY: [u8; 8] = [250, 234, 13, 123, 213, 156, 19, 236];
const SELL: [u8; 8] = [149, 39, 222, 155, 211, 124, 152, 26];

fn decode(account: &Account) -> Result<PoolState> {
    ensure!(
        account.owner == PROGRAM
            && account.data.get(..8) == Some(POOL_STATE_DISCRIMINATOR.as_slice()),
        "LaunchLab pool owner or discriminator mismatch"
    );
    pool_state_decode(account.data.get(8..).context("LaunchLab pool truncated")?)
        .context("LaunchLab pool layout invalid")
}

/// Reads the pair from authenticated account bytes without fetching any state.
pub(super) fn identity(account: &Account) -> Result<(Pubkey, Pubkey)> {
    let state = decode(account)?;
    Ok((state.base_mint, state.quote_mint))
}

/// Returns the complete market dependency set, including optionally absent fee vaults.
pub(super) fn dependencies(pool: Pubkey, account: &Account) -> Result<Vec<Pubkey>> {
    let state = decode(account)?;
    ensure!(
        pool == pda(PROGRAM, &[b"pool", state.base_mint.as_ref(), state.quote_mint.as_ref()]),
        "LaunchLab pool PDA mismatch"
    );
    Ok(vec![
        pool,
        state.base_mint,
        state.quote_mint,
        state.base_vault,
        state.quote_vault,
        state.global_config,
        state.platform_config,
        pda(PROGRAM, &[state.platform_config.as_ref(), state.quote_mint.as_ref()]),
        pda(PROGRAM, &[state.creator.as_ref(), state.quote_mint.as_ref()]),
    ])
}

/// Validated immutable quote state; the parent owns snapshot freshness and wallet setup.
#[derive(Clone)]
pub(super) struct State {
    pub mint: Pubkey,
    pub pool: Pubkey,
    pub quote_mint: Pubkey,
    pub base_reserve: u64,
    pub quote_reserve: u64,
    params: BonkParams,
    pub real_base_reserve: u64,
    pub real_quote_reserve: u64,
}

impl State {
    /// Validates a complete cache snapshot. Unsupported curves, extensions and migrated pools reject.
    pub(super) fn load(
        pool: Pubkey,
        mint: Pubkey,
        accounts: &HashMap<Pubkey, Option<Account>>,
    ) -> Result<Self> {
        let account = required(accounts, &pool)?;
        let state = decode(account)?;
        dependencies(pool, account)?;
        ensure!(state.base_mint == mint && mint != SOL, "LaunchLab base mint mismatch");
        ensure!(state.quote_mint == SOL, "LaunchLab non-SOL quote requires routing support");
        ensure!(state.status == 0, "LaunchLab pool has graduated or is not trading on its curve");
        ensure!(state.migrate_type <= 1, "Unsupported LaunchLab migration type");
        ensure!(
            state.total_base_sell > 0
                && state.real_base < state.total_base_sell
                && state.total_base_sell <= state.supply
                && state.virtual_base > state.real_base
                && state.virtual_quote > 0,
            "LaunchLab curve reserve invariant failed"
        );
        let base = required(accounts, &mint)?;
        let (_, decimals) = mint_info(base)?;
        let quote = required(accounts, &SOL)?;
        ensure!(
            quote.owner == TOKEN
                && quote.data.len() == 82
                && quote.data[45] == 1
                && quote.data[44] == 9,
            "LaunchLab WSOL mint layout invalid"
        );
        ensure!(
            decimals == state.base_decimals && state.quote_decimals == 9,
            "LaunchLab mint decimals mismatch"
        );
        let authority = pda(PROGRAM, &[b"vault_auth_seed"]);
        ensure!(
            Pubkey::find_program_address(&[b"vault_auth_seed"], &PROGRAM).1 == state.auth_bump,
            "LaunchLab authority bump mismatch"
        );
        ensure!(
            state.base_vault == pda(PROGRAM, &[b"pool_vault", pool.as_ref(), mint.as_ref()])
                && state.quote_vault == pda(PROGRAM, &[b"pool_vault", pool.as_ref(), SOL.as_ref()]),
            "LaunchLab vault PDA mismatch"
        );
        let available_base =
            token_amount(accounts, state.base_vault, mint, authority, base.owner, false)?;
        let available_quote =
            token_amount(accounts, state.quote_vault, SOL, authority, TOKEN, false)?;
        ensure!(
            available_base >= state.total_base_sell - state.real_base
                && available_quote >= state.real_quote,
            "LaunchLab vault reserves inconsistent"
        );
        let global = owned(accounts, state.global_config, PROGRAM, GLOBAL_CONFIG_DISC)?;
        let platform = owned(accounts, state.platform_config, PROGRAM, PLATFORM_CONFIG_DISC)?;
        let curve_type = *global.get(16).context("LaunchLab global config truncated")?;
        ensure!(curve_type == 0, "Unsupported LaunchLab curve type");
        let index: [u8; 2] = bytes(global, 17)?;
        ensure!(
            key_at(global, 83)? == SOL
                && state.global_config
                    == pda(PROGRAM, &[b"global_config", SOL.as_ref(), &[curve_type], &index]),
            "LaunchLab global config PDA or quote mint mismatch"
        );
        let fees = [u64_at(global, 27)?, u64_at(platform, 104)?, u64_at(platform, 720)?];
        ensure!(
            fees.iter().map(|fee| u128::from(*fee)).sum::<u128>() < 1_000_000,
            "LaunchLab fee rates exhaust input"
        );
        let platform_vault = pda(PROGRAM, &[state.platform_config.as_ref(), SOL.as_ref()]);
        let creator_vault = pda(PROGRAM, &[state.creator.as_ref(), SOL.as_ref()]);
        validate_fee_vault(accounts, platform_vault, b"platform_fee_vault_auth_seed")?;
        validate_fee_vault(accounts, creator_vault, b"creator_fee_vault_auth_seed")?;
        let base_reserve = state
            .virtual_base
            .checked_sub(state.real_base)
            .context("LaunchLab virtual base underflow")?;
        let quote_reserve = state
            .virtual_quote
            .checked_add(state.real_quote)
            .context("LaunchLab virtual quote overflow")?;
        let params = BonkParams {
            virtual_base: u128::from(state.virtual_base),
            virtual_quote: u128::from(state.virtual_quote),
            real_base: u128::from(state.real_base),
            real_quote: u128::from(state.real_quote),
            total_base_sell: u128::from(state.total_base_sell),
            pool_state: pool,
            base_vault: state.base_vault,
            quote_vault: state.quote_vault,
            mint_token_program: base.owner,
            quote_mint: SOL,
            quote_token_program: TOKEN,
            platform_config: state.platform_config,
            platform_associated_account: platform_vault,
            creator_associated_account: creator_vault,
            global_config: state.global_config,
            curve_type,
            trade_fee_rate: fees[0],
            platform_fee_rate: fees[1],
            creator_fee_rate: fees[2],
            ..BonkParams::default()
        };
        Ok(Self {
            mint,
            pool,
            quote_mint: SOL,
            base_reserve,
            quote_reserve,
            params,
            real_base_reserve: available_base,
            real_quote_reserve: available_quote,
        })
    }

    /// Quotes exact input from cached state and encodes the official zero-share-fee account order.
    /// Graduation may reduce a buy's actual input; the encoded amount preserves that cap.
    pub(super) fn instruction(
        &self,
        wallet: Pubkey,
        buy: bool,
        amount: u64,
        slippage_bps: u64,
    ) -> Result<Instruction> {
        ensure!(amount > 0 && slippage_bps < 10_000, "Invalid LaunchLab amount or slippage");
        let (actual_input, minimum) = if buy {
            let quote = get_buy_quote(amount, &self.params, 0, u128::from(slippage_bps))?;
            let unadjusted = get_buy_quote(amount, &self.params, 0, 0)?;
            ensure!(
                unadjusted.minimum_amount_out <= self.real_base_reserve,
                "LaunchLab base vault liquidity insufficient"
            );
            (quote.amount_in, quote.minimum_amount_out)
        } else {
            ensure!(
                u128::from(amount) <= self.params.real_base,
                "LaunchLab sell exceeds circulating curve base"
            );
            let gross_quote = u128::from(amount)
                .checked_mul(u128::from(self.quote_reserve))
                .and_then(|numerator| {
                    u128::from(self.base_reserve)
                        .checked_add(u128::from(amount))
                        .and_then(|denominator| numerator.checked_div(denominator))
                })
                .context("LaunchLab gross sell quote overflow")?;
            ensure!(
                gross_quote <= u128::from(self.real_quote_reserve)
                    && gross_quote <= self.params.real_quote,
                "LaunchLab quote liquidity insufficient"
            );
            (amount, get_sell_min_amount_out(amount, &self.params, 0, u128::from(slippage_bps))?)
        };
        ensure!(
            actual_input > 0 && actual_input <= amount && minimum > 0,
            "LaunchLab quote has zero output or invalid spend"
        );
        let p = &self.params;
        let metas = vec![
            AccountMeta::new(wallet, true),
            AccountMeta::new_readonly(pda(PROGRAM, &[b"vault_auth_seed"]), false),
            AccountMeta::new_readonly(p.global_config, false),
            AccountMeta::new_readonly(p.platform_config, false),
            AccountMeta::new(self.pool, false),
            AccountMeta::new(ata(wallet, self.mint, p.mint_token_program), false),
            AccountMeta::new(ata(wallet, self.quote_mint, TOKEN), false),
            AccountMeta::new(p.base_vault, false),
            AccountMeta::new(p.quote_vault, false),
            AccountMeta::new_readonly(self.mint, false),
            AccountMeta::new_readonly(self.quote_mint, false),
            AccountMeta::new_readonly(p.mint_token_program, false),
            AccountMeta::new_readonly(TOKEN, false),
            AccountMeta::new_readonly(pda(PROGRAM, &[b"__event_authority"]), false),
            AccountMeta::new_readonly(PROGRAM, false),
            AccountMeta::new_readonly(SYSTEM, false),
            AccountMeta::new(p.platform_associated_account, false),
            AccountMeta::new(p.creator_associated_account, false),
        ];
        let mut data = Vec::with_capacity(32);
        data.extend(if buy { BUY } else { SELL });
        data.extend(actual_input.to_le_bytes());
        data.extend(minimum.to_le_bytes());
        data.extend(0_u64.to_le_bytes());
        Ok(Instruction { program_id: PROGRAM, accounts: metas, data })
    }

    /// Returns rounded basis points for display only; quotes retain on-chain millionth rates.
    pub(super) fn fee_bps(&self) -> [u64; 3] {
        [self.params.trade_fee_rate, self.params.platform_fee_rate, self.params.creator_fee_rate]
            .map(|rate| (rate + 50) / 100)
    }
}

fn validate_fee_vault(
    accounts: &HashMap<Pubkey, Option<Account>>,
    vault: Pubkey,
    authority_seed: &[u8],
) -> Result<()> {
    let account = accounts.get(&vault).context("LaunchLab fee vault was not fetched")?;
    if let Some(account) = account {
        if account.owner == SYSTEM && account.data.is_empty() {
            return Ok(());
        }
        token_amount(accounts, vault, SOL, pda(PROGRAM, &[authority_seed]), TOKEN, false)?;
    }
    Ok(())
}
