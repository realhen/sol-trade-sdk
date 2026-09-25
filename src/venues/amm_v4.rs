//! Raydium AMM v4 V2 swaps use checked vault-minus-PNL reserves without OpenBook accounts.

use super::*;
use crate::instruction::utils::raydium_amm_v4_types::{amm_info_decode, AmmInfo, AMM_INFO_SIZE};

/// Mainnet Raydium AMM v4 program.
pub(super) const PROGRAM: Pubkey =
    solana_sdk::pubkey!("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8");
const CLOCK: Pubkey = solana_sdk::pubkey!("SysvarC1ock11111111111111111111111111111111");
const SYSVAR: Pubkey = solana_sdk::pubkey!("Sysvar1111111111111111111111111111111111111");

fn decode(account: &Account) -> Result<AmmInfo> {
    ensure!(
        account.owner == PROGRAM && account.data.len() == AMM_INFO_SIZE,
        "Unsupported Raydium AMM v4 owner or layout"
    );
    let decoded = amm_info_decode(&account.data).context("Invalid Raydium AMM v4 pool")?;
    ensure!(
        (1..=7).contains(&decoded.status)
            && decoded.nonce <= u64::from(u8::MAX)
            && decoded.state <= 6
            && decoded.reset_flag <= 1
            && decoded.coin_mint != decoded.pc_mint
            && decoded.coin_mint != SYSTEM
            && decoded.pc_mint != SYSTEM,
        "Invalid Raydium AMM v4 identity or state"
    );
    // The SDK's first final padding word is the current program's recent_epoch.
    ensure!(decoded.padding[1] == 0, "Unsupported Raydium AMM v4 reserved state");
    Ok(decoded)
}

/// Returns authenticated coin/PC mints; discovery does not imply swap permission.
pub(super) fn identity(account: &Account) -> Result<(Pubkey, Pubkey)> {
    let decoded = decode(account)?;
    Ok((decoded.coin_mint, decoded.pc_mint))
}

/// Required confirmed account dependencies for V2 swaps; orderbook balances are never quoted.
pub(super) fn dependencies(pool: Pubkey, account: &Account) -> Result<Vec<Pubkey>> {
    let decoded = decode(account)?;
    Ok(vec![pool, decoded.coin_mint, decoded.pc_mint, decoded.token_coin, decoded.token_pc, CLOCK])
}

/// Validated immutable snapshot, with public reserves oriented as traded mint/WSOL.
#[derive(Clone)]
pub(super) struct State {
    pub mint: Pubkey,
    pub pool: Pubkey,
    pub quote_mint: Pubkey,
    pub base_reserve: u64,
    pub quote_reserve: u64,
    pub real_base_reserve: u64,
    pub real_quote_reserve: u64,
    decoded: AmmInfo,
    authority: Pubkey,
    coin_reserve: u64,
    pc_reserve: u64,
    coin_vault_amount: u64,
    pc_vault_amount: u64,
}

impl State {
    /// Authenticates cached pool/PDA/vault relationships and the program's swap permission rules.
    /// Only classic SPL-token pairs containing WSOL are executable. No RPC or signer is used.
    pub(super) fn load(
        pool: Pubkey,
        mint: Pubkey,
        accounts: &HashMap<Pubkey, Option<Account>>,
    ) -> Result<Self> {
        let decoded = decode(required(accounts, &pool)?)?;
        ensure!(mint != SOL && mint != SYSTEM, "Unsupported Raydium AMM v4 traded mint");
        let quote_mint = if mint == decoded.coin_mint {
            decoded.pc_mint
        } else {
            ensure!(mint == decoded.pc_mint, "AMM v4 pool does not contain requested mint");
            decoded.coin_mint
        };
        ensure!(quote_mint == SOL, "AMM v4 execution currently requires a WSOL quote");
        ensure!(matches!(decoded.status, 1 | 6 | 7), "Raydium AMM v4 swaps are disabled");
        let clock = required(accounts, &CLOCK)?;
        ensure!(clock.owner == SYSVAR && clock.data.len() == 40, "Invalid Clock sysvar");
        let timestamp = i64::from_le_bytes(bytes(&clock.data, 32)?);
        ensure!(timestamp >= 0, "Invalid Clock timestamp");
        if decoded.status == 7 {
            ensure!(timestamp as u64 >= decoded.out_put.pool_open_time, "AMM v4 pool is not open");
        }
        ensure!(
            decoded.fees.swap_fee_denominator > 0
                && decoded.fees.swap_fee_numerator < decoded.fees.swap_fee_denominator,
            "Invalid AMM v4 swap fee fraction"
        );
        let authority =
            Pubkey::create_program_address(&[b"amm authority", &[decoded.nonce as u8]], &PROGRAM)
                .context("Invalid AMM v4 authority nonce")?;
        // Migration changed the market key while preserving original vaults and pool addresses.
        // Validate the program-owned pool's vault links and authority instead of rederiving from market.
        let mut balances = [0; 2];
        for (index, (token_mint, vault, decimals)) in [
            (decoded.coin_mint, decoded.token_coin, decoded.coin_decimals),
            (decoded.pc_mint, decoded.token_pc, decoded.pc_decimals),
        ]
        .into_iter()
        .enumerate()
        {
            let mint_account = required(accounts, &token_mint)?;
            ensure!(mint_account.owner == TOKEN, "AMM v4 requires classic SPL tokens");
            let actual_decimals = if token_mint == SOL {
                ensure!(
                    mint_account.data.len() == 82
                        && mint_account.data[45] == 1
                        && mint_account.data[44] == 9,
                    "Invalid WSOL mint"
                );
                9
            } else {
                mint_info(mint_account)?.1
            };
            ensure!(u64::from(actual_decimals) == decimals, "AMM v4 mint decimals mismatch");
            balances[index] = token_amount(accounts, vault, token_mint, authority, TOKEN, false)?;
            let vault_account = required(accounts, &vault)?;
            ensure!(
                u32::from_le_bytes(bytes(&vault_account.data, 72)?) == 0
                    && u32::from_le_bytes(bytes(&vault_account.data, 129)?) == 0,
                "Delegated or externally closable AMM v4 vault unsupported"
            );
        }
        let coin_reserve = balances[0]
            .checked_sub(decoded.out_put.need_take_pnl_coin)
            .context("AMM v4 coin PNL exceeds vault balance")?;
        let pc_reserve = balances[1]
            .checked_sub(decoded.out_put.need_take_pnl_pc)
            .context("AMM v4 PC PNL exceeds vault balance")?;
        ensure!(coin_reserve > 0 && pc_reserve > 0, "AMM v4 pool has no usable liquidity");
        let (base_reserve, quote_reserve) = if mint == decoded.coin_mint {
            (coin_reserve, pc_reserve)
        } else {
            (pc_reserve, coin_reserve)
        };
        let (real_base_reserve, real_quote_reserve) = if mint == decoded.coin_mint {
            (balances[0], balances[1])
        } else {
            (balances[1], balances[0])
        };
        Ok(Self {
            real_base_reserve,
            real_quote_reserve,
            mint,
            pool,
            quote_mint,
            base_reserve,
            quote_reserve,
            decoded,
            authority,
            coin_reserve,
            pc_reserve,
            coin_vault_amount: balances[0],
            pc_vault_amount: balances[1],
        })
    }

    /// Builds the eight-account SwapBaseInV2 instruction from cached state in atomic units.
    /// The fee uses the pool's swap fraction with ceiling rounding; output and slippage round down.
    pub(super) fn instruction(
        &self,
        wallet: Pubkey,
        buy: bool,
        amount: u64,
        slippage_bps: u64,
    ) -> Result<Instruction> {
        ensure!(amount > 0 && slippage_bps < 10_000, "Invalid AMM v4 amount or slippage");
        let input_mint = if buy { self.quote_mint } else { self.mint };
        let output_mint = if buy { self.mint } else { self.quote_mint };
        let (input_reserve, output_reserve, input_balance) = if input_mint == self.decoded.coin_mint
        {
            ensure!(output_mint == self.decoded.pc_mint, "Invalid AMM v4 output mint");
            (self.coin_reserve, self.pc_reserve, self.coin_vault_amount)
        } else {
            ensure!(
                input_mint == self.decoded.pc_mint && output_mint == self.decoded.coin_mint,
                "Invalid AMM v4 swap pair"
            );
            (self.pc_reserve, self.coin_reserve, self.pc_vault_amount)
        };
        input_balance.checked_add(amount).context("AMM v4 input vault would overflow")?;
        let fee = u128::from(amount)
            .checked_mul(u128::from(self.decoded.fees.swap_fee_numerator))
            .context("AMM v4 fee overflow")?
            .div_ceil(u128::from(self.decoded.fees.swap_fee_denominator));
        let net_input = u128::from(amount)
            .checked_sub(fee)
            .filter(|value| *value > 0)
            .context("AMM v4 input is consumed by fees")?;
        let denominator =
            u128::from(input_reserve).checked_add(net_input).context("AMM v4 reserve overflow")?;
        let output = u128::from(output_reserve)
            .checked_mul(net_input)
            .and_then(|value| value.checked_div(denominator))
            .context("Invalid AMM v4 quote")?;
        ensure!(
            output > 0 && output < u128::from(output_reserve),
            "AMM v4 output is outside usable liquidity"
        );
        let minimum = output
            .checked_mul(u128::from(10_000 - slippage_bps))
            .and_then(|value| value.checked_div(10_000))
            .context("AMM v4 slippage arithmetic overflow")?;
        let minimum = u64::try_from(minimum).context("AMM v4 minimum exceeds u64")?;
        ensure!(minimum > 0, "AMM v4 minimum output rounds to zero");
        let source = ata(wallet, input_mint, TOKEN);
        let destination = ata(wallet, output_mint, TOKEN);
        ensure!(
            source != self.decoded.token_coin
                && source != self.decoded.token_pc
                && destination != self.decoded.token_coin
                && destination != self.decoded.token_pc,
            "AMM v4 user account aliases a vault"
        );
        let mut data = Vec::with_capacity(17);
        data.push(16);
        data.extend_from_slice(&amount.to_le_bytes());
        data.extend_from_slice(&minimum.to_le_bytes());
        Ok(Instruction {
            program_id: PROGRAM,
            accounts: vec![
                AccountMeta::new_readonly(TOKEN, false),
                AccountMeta::new(self.pool, false),
                AccountMeta::new_readonly(self.authority, false),
                AccountMeta::new(self.decoded.token_coin, false),
                AccountMeta::new(self.decoded.token_pc, false),
                AccountMeta::new(source, false),
                AccountMeta::new(destination, false),
                AccountMeta::new_readonly(wallet, true),
            ],
            data,
        })
    }

    /// Total swap fee in the first display slot, rounded upward to basis points.
    /// Delayed PNL distribution is not an additional per-swap fee; execution uses the exact fraction.
    pub(super) fn fee_bps(&self) -> [u64; 3] {
        [
            (u128::from(self.decoded.fees.swap_fee_numerator) * 10_000)
                .div_ceil(u128::from(self.decoded.fees.swap_fee_denominator)) as u64,
            0,
            0,
        ]
    }
}
