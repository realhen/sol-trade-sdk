//! Offline builder workflow checked independently by the official Pump TypeScript SDK.
use serde_json::json;
use sol_trade_sdk::{
    common::{bonding_curve::BondingCurveAccount, GasFeeStrategy},
    constants::{TOKEN_PROGRAM, TOKEN_PROGRAM_2022},
    instruction::pumpfun::PumpFunInstructionBuilder,
    trading::core::{
        params::{DexParamEnum, PumpFunParams, SwapParams},
        traits::InstructionBuilder,
    },
};
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer};
use std::{
    io::Write,
    process::{Command, Stdio},
    sync::Arc,
};
fn swap_params_for_buy(mint: Pubkey, token_program: Pubkey) -> SwapParams {
    let bonding_curve =
        sol_trade_sdk::instruction::utils::pumpfun::get_bonding_curve_pda(&mint).unwrap();
    let creator = Pubkey::new_unique();
    let creator_vault =
        sol_trade_sdk::instruction::utils::pumpfun::get_creator_vault_pda(&creator).unwrap();
    let bc = BondingCurveAccount {
            account: bonding_curve,
            virtual_token_reserves: sol_trade_sdk::instruction::utils::pumpfun::global_constants::INITIAL_VIRTUAL_TOKEN_RESERVES,
            virtual_sol_reserves: sol_trade_sdk::instruction::utils::pumpfun::global_constants::INITIAL_VIRTUAL_SOL_RESERVES,
            real_token_reserves: sol_trade_sdk::instruction::utils::pumpfun::global_constants::INITIAL_REAL_TOKEN_RESERVES,
            creator,
            ..Default::default()
        };
    let params = PumpFunParams {
        bonding_curve: Arc::new(bc),
        associated_bonding_curve: Pubkey::default(),
        observed_trade_creator: Some(creator),
        creator_vault,
        fee_sharing_creator_vault_if_active: None,
        token_program,
        close_token_account_when_sell: None,
        fee_recipient: sol_trade_sdk::instruction::utils::pumpfun::global_constants::FEE_RECIPIENT,
        quote_mint: Pubkey::default(),
    };

    SwapParams {
        rpc: None,
        payer: Arc::new(Keypair::new()),
        trade_type: sol_trade_sdk::swqos::TradeType::Buy,
        input_mint: sol_trade_sdk::constants::SOL_TOKEN_ACCOUNT,
        input_token_program: None,
        output_mint: mint,
        output_token_program: None,
        input_amount: Some(10_000_000),
        slippage_basis_points: Some(300),
        address_lookup_table_accounts: Vec::new(),
        recent_blockhash: None,
        wait_tx_confirmed: false,
        protocol_params: DexParamEnum::PumpFun(params),
        open_seed_optimize: true,
        swqos_clients: Arc::new(Vec::new()),
        middleware_manager: None,
        durable_nonce: None,
        with_tip: true,
        create_input_mint_ata: false,
        close_input_mint_ata: false,
        create_output_mint_ata: true,
        close_output_mint_ata: false,
        fixed_output_amount: None,
        gas_fee_strategy: GasFeeStrategy::new(),
        simulate: false,
        log_enabled: false,
        wait_for_all_submits: false,
        use_dedicated_sender_threads: false,
        sender_thread_cores: None,
        max_sender_concurrency: 0,
        effective_core_ids: Arc::new(Vec::new()),
        check_min_tip: false,
        transaction_version: sol_trade_sdk::common::TradeTransactionVersion::V0,
        grpc_recv_us: None,
        use_exact_sol_amount: Some(true),
    }
}

#[tokio::test]
#[ignore = "requires pinned official SDK oracle; npm ci --ignore-scripts in validation"]
async fn pump_instruction_bytes_and_accounts_match_official_sdk() {
    let mut cases = Vec::new();
    let mint: Pubkey = "E3JvmGcGFDzhu2Cnxyeq5BRvN7HH9JZUsfAUh2v8pump".parse().unwrap();
    for token_program in [TOKEN_PROGRAM, TOKEN_PROGRAM_2022] {
        for v2 in [false, true] {
            for cashback in [false, true] {
                for sell in [false, true] {
                    let mut p = swap_params_for_buy(mint, token_program);
                    p.open_seed_optimize = false;
                    p.create_output_mint_ata = false;
                    p.fixed_output_amount = Some(1_000);
                    let quote = if v2 {
                        "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".parse().unwrap()
                    } else {
                        sol_trade_sdk::constants::SOL_TOKEN_ACCOUNT
                    };
                    let DexParamEnum::PumpFun(ref mut protocol) = p.protocol_params else {
                        unreachable!()
                    };
                    let curve = Arc::make_mut(&mut protocol.bonding_curve);
                    curve.is_cashback_coin = cashback;
                    if v2 {
                        curve.quote_mint = quote;
                        protocol.quote_mint = quote;
                    }
                    let creator = curve.creator;
                    let fee_recipient = protocol.fee_recipient;
                    if sell {
                        p.trade_type = sol_trade_sdk::swqos::TradeType::Sell;
                        p.input_mint = mint;
                        p.output_mint = quote;
                    } else {
                        p.input_mint = quote;
                    }
                    let ixs = if sell {
                        PumpFunInstructionBuilder.build_sell_instructions(&p).await.unwrap()
                    } else {
                        PumpFunInstructionBuilder.build_buy_instructions(&p).await.unwrap()
                    };
                    let ix = ixs
                        .iter()
                        .find(|ix| {
                            ix.program_id
                                == sol_trade_sdk::instruction::utils::pumpfun::accounts::PUMPFUN
                        })
                        .unwrap();
                    let buyback =
                        if v2 { ix.accounts[8].pubkey } else { ix.accounts.last().unwrap().pubkey };
                    cases.push(json!({"v2":v2,"sell":sell,"cashback":cashback,"user":p.payer.pubkey().to_string(),"mint":mint.to_string(),"creator":creator.to_string(),"feeRecipient":fee_recipient.to_string(),"buybackFeeRecipient":buyback.to_string(),"tokenProgram":token_program.to_string(),"quoteMint":quote.to_string(),"data":ix.data,"accounts":ix.accounts.iter().map(|m|json!([m.pubkey.to_string(),m.is_signer,m.is_writable])).collect::<Vec<_>>()}));
                }
            }
        }
    }
    let invalid_owner = swap_params_for_buy(mint, Pubkey::new_unique());
    assert!(PumpFunInstructionBuilder.build_buy_instructions(&invalid_owner).await.is_err());
    let mut child = Command::new("node")
        .arg(format!("{}/validation/pump-builder-oracle.cjs", env!("CARGO_MANIFEST_DIR")))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("node installed");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(&cases).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    println!("{}", String::from_utf8_lossy(&output.stdout));
}
