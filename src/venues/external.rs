//! Pure venue dispatch around validated, streamed accounts. No custody or network clients.
use super::*;

#[derive(Clone)]
pub(super) enum State {
    Cpmm(Arc<cpmm::State>),
    AmmV4(Arc<amm_v4::State>),
    LaunchLab(Arc<launchlab::State>),
    Dbc(Arc<dbc::State>),
    DammV2(Arc<damm_v2::State>),
}

pub(super) fn supports(program: Pubkey) -> bool {
    program == cpmm::PROGRAM
        || program == launchlab::PROGRAM
        || program == amm_v4::PROGRAM
        || program == dbc::PROGRAM
        || program == damm_v2::PROGRAM
}

pub(super) fn identity(account: &Account) -> Result<(Pubkey, Pubkey)> {
    if account.owner == cpmm::PROGRAM {
        cpmm::identity(account)
    } else if account.owner == amm_v4::PROGRAM {
        amm_v4::identity(account)
    } else if account.owner == launchlab::PROGRAM {
        launchlab::identity(account)
    } else if account.owner == dbc::PROGRAM {
        dbc::identity(account)
    } else if account.owner == damm_v2::PROGRAM {
        damm_v2::identity(account)
    } else {
        bail!("Unsupported pool program")
    }
}

pub(super) fn dependencies(pool: Pubkey, account: &Account) -> Result<Vec<Pubkey>> {
    if account.owner == cpmm::PROGRAM {
        cpmm::dependencies(pool, account)
    } else if account.owner == amm_v4::PROGRAM {
        amm_v4::dependencies(pool, account)
    } else if account.owner == launchlab::PROGRAM {
        launchlab::dependencies(pool, account)
    } else if account.owner == dbc::PROGRAM {
        dbc::dependencies(pool, account)
    } else if account.owner == damm_v2::PROGRAM {
        damm_v2::dependencies(pool, account)
    } else {
        bail!("Unsupported pool program")
    }
}

pub(super) fn load(
    mint: Pubkey,
    pool: Pubkey,
    accounts: &HashMap<Pubkey, Option<Account>>,
) -> Result<MarketState> {
    let account = required(accounts, &pool)?;
    let (state, base_reserve, quote_reserve, fees) = if account.owner == cpmm::PROGRAM {
        let state = cpmm::State::load(pool, mint, accounts)?;
        let values = (state.base_reserve, state.quote_reserve, state.fee_bps());
        (State::Cpmm(Arc::new(state)), values.0, values.1, values.2)
    } else if account.owner == amm_v4::PROGRAM {
        let state = amm_v4::State::load(pool, mint, accounts)?;
        let values = (state.base_reserve, state.quote_reserve, state.fee_bps());
        (State::AmmV4(Arc::new(state)), values.0, values.1, values.2)
    } else if account.owner == dbc::PROGRAM {
        let state = dbc::State::load(pool, mint, accounts)?;
        let values = (state.base_reserve, state.quote_reserve, state.fee_bps());
        (State::Dbc(Arc::new(state)), values.0, values.1, values.2)
    } else if account.owner == damm_v2::PROGRAM {
        let state = damm_v2::State::load(pool, mint, accounts)?;
        let values = (state.base_reserve, state.quote_reserve, state.fee_bps());
        (State::DammV2(Arc::new(state)), values.0, values.1, values.2)
    } else if account.owner == launchlab::PROGRAM {
        let state = launchlab::State::load(pool, mint, accounts)?;
        let values = (state.base_reserve, state.quote_reserve, state.fee_bps());
        (State::LaunchLab(Arc::new(state)), values.0, values.1, values.2)
    } else {
        bail!("Unsupported pool program")
    };
    let (real_base, real_quote) = match &state {
        State::Cpmm(state) => (state.real_base_reserve, state.real_quote_reserve),
        State::AmmV4(state) => (state.real_base_reserve, state.real_quote_reserve),
        State::LaunchLab(state) => (state.real_base_reserve, state.real_quote_reserve),
        State::Dbc(state) => (state.real_base_reserve, state.real_quote_reserve),
        State::DammV2(state) => (state.real_base_reserve, state.real_quote_reserve),
    };
    let token_program = required(accounts, &mint)?.owner;
    mint_info(required(accounts, &mint)?)?;
    Ok(MarketState {
        mint,
        token_program,
        pool,
        creator: SYSTEM,
        fee_recipient: SYSTEM,
        buyback: SYSTEM,
        base_reserve,
        quote_reserve,
        real_base,
        real_quote,
        fees,
        sell_fees: fees,
        disabled: 0,
        amm: true,
        extend_pool: false,
        virtual_quote: if matches!(state, State::LaunchLab(_) | State::Dbc(_)) {
            i128::from(quote_reserve) - i128::from(real_quote)
        } else {
            0
        },
        execution_error: None,
        external: Some(state),
    })
}

pub(super) fn build(
    state: &State,
    wallet: Pubkey,
    mint: Pubkey,
    token_program: Pubkey,
    buy: bool,
    amount: u64,
    slippage_bps: u64,
) -> Result<Vec<Instruction>> {
    let swap = match state {
        State::Cpmm(state) => state.instruction(wallet, buy, amount, slippage_bps)?,
        State::AmmV4(state) => state.instruction(wallet, buy, amount, slippage_bps)?,
        State::LaunchLab(state) => state.instruction(wallet, buy, amount, slippage_bps)?,
        State::Dbc(state) => state.instruction(wallet, buy, amount, slippage_bps)?,
        State::DammV2(state) => state.instruction(wallet, buy, amount, slippage_bps)?,
    };
    let mut instructions = vec![
        create_ata(wallet, wallet, mint, token_program),
        create_ata(wallet, wallet, SOL, TOKEN),
    ];
    let wsol = ata(wallet, SOL, TOKEN);
    if buy {
        instructions.push(solana_system_interface::instruction::transfer(&wallet, &wsol, amount));
        instructions.push(Instruction {
            program_id: TOKEN,
            accounts: vec![rw(wsol)],
            data: vec![17],
        });
    }
    instructions.push(swap);
    instructions.push(Instruction {
        program_id: TOKEN,
        accounts: vec![rw(wsol), rw(wallet), signer(wallet)],
        data: vec![9],
    });
    Ok(instructions)
}

/// Reports the actual decoder family used by this immutable snapshot.
pub(super) fn parser(state: &State) -> &'static str {
    match state {
        State::Dbc(_) | State::DammV2(_) => "sol-trade-sdk/keyless-venues",
        _ => "sol-trade-sdk/5.0.5",
    }
}

/// Exact raw SOL/token spot ratio for concentrated curves; ordinary venues use reserve ratios.
pub(super) fn spot_price_ratio(
    state: &State,
) -> Option<(num_bigint::BigUint, num_bigint::BigUint)> {
    match state {
        State::Dbc(state) => Some(state.spot_price_ratio()),
        State::DammV2(state) => Some(state.spot_price_ratio()),
        _ => None,
    }
}
