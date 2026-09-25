//! Unverified CLMM instruction previews; no account-state validation or execution authority.
use super::{TOKEN, TOKEN_2022};
use crate::instruction::raydium_clmm::{swap_v2, RaydiumClmmSwapV2Accounts, RaydiumClmmSwapV2Args};
use anyhow::{ensure, Result};
use solana_sdk::{instruction::Instruction, pubkey::Pubkey};
use std::collections::HashSet;
const CLMM_PROGRAM: Pubkey = solana_sdk::pubkey!("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK");
const MEMO_PROGRAM: Pubkey = solana_sdk::pubkey!("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr");
const SWAP_V2_DISCRIMINATOR: [u8; 8] = [43, 4, 237, 11, 26, 201, 30, 98];

/// Public unverified account addresses for the restricted exact-input preview.
pub struct PreviewAccounts {
    /// Public payer identity; must be on curve.
    pub payer: Pubkey,
    /// Unverified AMM configuration.
    pub amm_config: Pubkey,
    /// Unverified pool state.
    pub pool_state: Pubkey,
    /// Unverified input token account.
    pub input_token_account: Pubkey,
    /// Unverified output token account.
    pub output_token_account: Pubkey,
    /// Unverified input vault.
    pub input_vault: Pubkey,
    /// Unverified output vault.
    pub output_vault: Pubkey,
    /// Unverified observation account.
    pub observation_state: Pubkey,
    /// Unverified input mint.
    pub input_mint: Pubkey,
    /// Unverified output mint.
    pub output_mint: Pubkey,
    /// One to eight unverified tick-array addresses.
    pub tick_arrays: Vec<Pubkey>,
}

/// Builds a restricted exact-input instruction and verifies its encoding and privileges.
/// Account relationships and economics remain unverified: do not treat this as an executable market.
pub fn build_unverified_preview(
    a: &PreviewAccounts,
    amount: u64,
    threshold: u64,
    limit: u128,
) -> Result<Instruction> {
    ensure!((1..=8).contains(&a.tick_arrays.len()), "Supply 1–8 tick arrays");
    ensure!(a.payer.is_on_curve(), "Payer must be an on-curve public key");
    let accounts = RaydiumClmmSwapV2Accounts {
        payer: a.payer,
        amm_config: a.amm_config,
        pool_state: a.pool_state,
        input_token_account: a.input_token_account,
        output_token_account: a.output_token_account,
        input_vault: a.input_vault,
        output_vault: a.output_vault,
        observation_state: a.observation_state,
        token_program: TOKEN,
        token_program_2022: TOKEN_2022,
        input_vault_mint: a.input_mint,
        output_vault_mint: a.output_mint,
        tick_array_bitmap_extension: None,
        tick_arrays: a.tick_arrays.clone(),
    };
    ensure!(
        accounts.input_vault_mint != accounts.output_vault_mint,
        "Input and output mints must differ"
    );
    ensure!(amount > 0 && threshold > 0, "Amount and minimum output must be positive");
    ensure!(
        limit == 0 || (4_295_048_017..79_226_673_521_066_979_257_578_248_091).contains(&limit),
        "Square-root price limit outside protocol bounds"
    );
    let instruction = swap_v2(
        &accounts,
        RaydiumClmmSwapV2Args {
            amount,
            other_amount_threshold: threshold,
            sqrt_price_limit_x64: limit,
            is_base_input: true,
        },
    )?;
    validate_instruction(&instruction, &accounts, amount, threshold, limit)?;
    Ok(instruction)
}

fn validate_instruction(
    ix: &Instruction,
    a: &RaydiumClmmSwapV2Accounts,
    amount: u64,
    threshold: u64,
    limit: u128,
) -> Result<()> {
    ensure!(ix.program_id == CLMM_PROGRAM, "SDK returned unexpected program");
    let mut expected = vec![
        (a.payer, true, false),
        (a.amm_config, false, false),
        (a.pool_state, false, true),
        (a.input_token_account, false, true),
        (a.output_token_account, false, true),
        (a.input_vault, false, true),
        (a.output_vault, false, true),
        (a.observation_state, false, true),
        (TOKEN, false, false),
        (TOKEN_2022, false, false),
        (MEMO_PROGRAM, false, false),
        (a.input_vault_mint, false, false),
        (a.output_vault_mint, false, false),
    ];
    expected.extend(a.tick_arrays.iter().map(|key| (*key, false, true)));
    ensure!(
        expected.iter().map(|v| v.0).collect::<HashSet<_>>().len() == expected.len(),
        "Account aliases are unsupported"
    );
    ensure!(ix.accounts.len() == expected.len(), "SDK changed instruction account count");
    ensure!(
        ix.accounts
            .iter()
            .zip(expected)
            .all(|(actual, (address, signer, writable))| actual.pubkey == address
                && actual.is_signer == signer
                && actual.is_writable == writable),
        "SDK changed account privileges or recipients"
    );
    let mut data = Vec::from(SWAP_V2_DISCRIMINATOR);
    data.extend(amount.to_le_bytes());
    data.extend(threshold.to_le_bytes());
    data.extend(limit.to_le_bytes());
    data.push(1);
    ensure!(ix.data == data, "SDK changed amounts, limits, or instruction discriminator");
    Ok(())
}
