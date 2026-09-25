//! Checked, bounded Pump account decoding without network access.
use anyhow::{bail, ensure, Context, Result};
use sol_parser_sdk::{
    accounts::{parse_account_unified, AccountData},
    core::events::{PumpFunBondingCurve, PumpFunFeeConfig, PumpSwapPool},
    DexEvent, EventMetadata,
};
use solana_sdk::pubkey::Pubkey;

/// Public identity of the pinned account decoder.
pub const PARSER: &str = "sol-parser-sdk/0.7.6";

fn decode(owner: Pubkey, data: &[u8]) -> Result<DexEvent> {
    parse_account_unified(
        &AccountData {
            pubkey: Pubkey::default(),
            owner,
            data: data.to_vec(),
            executable: false,
            lamports: 0,
            rent_epoch: 0,
        },
        EventMetadata::default(),
        None,
    )
    .context("Fnzero could not decode supported account layout")
}

/// Decodes complete historical fields or a current layout with zero allocation padding.
/// The caller verifies owner/discriminator; partial fields and unknown extensions reject.
pub fn curve(owner: Pubkey, data: &[u8]) -> Result<PumpFunBondingCurve> {
    ensure!(
        [49, 81, 82, 83, 115, 123, 124].contains(&data.len()) || data.len() >= 125,
        "Unsupported bonding curve layout"
    );
    ensure!(
        data.get(125..).is_none_or(|padding| padding.iter().all(|byte| *byte == 0)),
        "Unsupported bonding curve extension data"
    );
    for offset in [48, 81, 82, 123, 124] {
        ensure!(data.get(offset).is_none_or(|value| *value <= 1), "Invalid bonding curve flag");
    }
    match decode(owner, data)? {
        DexEvent::PumpFunBondingCurveAccount(event) => Ok(event.bonding_curve),
        _ => bail!("Unexpected fnzero bonding curve type"),
    }
}

/// Decodes complete historical pool fields or a zero-padded current layout.
/// Partial fields, nonzero reserved padding and malformed boolean fields reject.
pub fn pool(owner: Pubkey, data: &[u8]) -> Result<PumpSwapPool> {
    ensure!(
        [211, 243, 244, 245, 261, 269, 270].contains(&data.len())
            || data.len() >= 271
            || (data.len() == 252 && data[245..].iter().all(|byte| *byte == 0)),
        "Unsupported PumpSwap pool layout"
    );
    ensure!(
        data.get(271..).is_none_or(|padding| padding.iter().all(|byte| *byte == 0)),
        "Unsupported pool extension data"
    );
    for offset in [243, 244, 269, 270] {
        ensure!(data.get(offset).is_none_or(|value| *value <= 1), "Invalid pool flag");
    }
    match decode(owner, data)? {
        DexEvent::PumpSwapPoolAccount(event) => Ok(event.pool),
        _ => bail!("Unexpected fnzero pool type"),
    }
}

/// Decodes bounded fee vectors; the caller still validates fee policy and tier ordering.
pub fn fees(owner: Pubkey, data: &[u8]) -> Result<PumpFunFeeConfig> {
    ensure!(data.len() >= 2512 && data.len() <= 512 * 1024, "Unsupported fee config layout");
    match decode(owner, data)? {
        DexEvent::PumpFunFeeConfigAccount(event) => Ok(event.fee_config),
        _ => bail!("Unexpected fnzero fee config type"),
    }
}
