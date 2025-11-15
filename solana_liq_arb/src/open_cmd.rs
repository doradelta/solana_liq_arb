use anyhow::{bail, Context, Result};
use carbon_core::borsh::{self, BorshSerialize};
use carbon_core::deserialize::CarbonDeserialize;
use carbon_raydium_clmm_decoder::instructions::open_position_v2::{
    OpenPositionV2,
};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    instruction::AccountMeta,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    system_program, sysvar,
    transaction::Transaction,
};
use spl_associated_token_account::get_associated_token_address_with_program_id;
use spl_token::ID as SPL_TOKEN_PROGRAM_ID;
use std::str::FromStr;

use crate::keypair_loader::load_keypair;
use crate::pda::METADATA_PROGRAM_ID;
use crate::pool::{price_to_tick, tick_array_start};
use crate::pool_cache;

const SPL_TOKEN_2022_PROGRAM_ID: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

pub async fn run_open(
    rpc_url: &str,
    payer_path: &str,
    pool_str: &str,
    price_min: Option<f64>,
    price_max: Option<f64>,
    tick_lower: Option<i32>,
    tick_upper: Option<i32>,
    amount0_max: u64,
    amount1_max: u64,
) -> Result<()> {
    // Blocking RPC client (simpler; acceptable for CLI)
    let rpc = RpcClient::new_with_commitment(rpc_url.to_string(), CommitmentConfig::confirmed());
    let payer = load_keypair(payer_path).context("load payer (file or Phantom base58/JSON)")?;
    let pool = Pubkey::from_str(pool_str).context("pool pubkey")?;

    // Fetch cached pool info (program id, mints, vaults, tick spacing)
    let pool_snap = pool_cache::get_or_fetch_sync(&rpc, &pool, false)
        .context("read pool snapshot (pool-cache). run cache-pool if missing")?;
    let mut program_id = Pubkey::from_str(&pool_snap.program_id).unwrap_or(Pubkey::new_from_array([0u8; 32]));
    if program_id == Pubkey::new_from_array([0u8; 32]) {
        // Refresh owner if cache was created before program_id was recorded
        program_id = rpc.get_account(&pool)?.owner;
    }
    let mint0 = Pubkey::from_str(&pool_snap.token_mint0)?;
    let mint1 = Pubkey::from_str(&pool_snap.token_mint1)?;
    let vault0 = Pubkey::from_str(&pool_snap.token_vault0)?;
    let vault1 = Pubkey::from_str(&pool_snap.token_vault1)?;
    let tick_spacing = pool_snap.tick_spacing as i32;

    // Derive ticks
    let (t_lower, t_upper) = match (tick_lower, tick_upper, price_min, price_max) {
        (Some(a), Some(b), _, _) => (a, b),
        (_, _, Some(pmin), Some(pmax)) => (price_to_tick(pmin), price_to_tick(pmax)),
        _ => bail!("supply either --tick-lower/--tick-upper or --price-min/--price-max"),
    };
    if t_lower >= t_upper {
        bail!("tick_lower must be < tick_upper");
    }
    if t_lower % tick_spacing != 0 || t_upper % tick_spacing != 0 {
        bail!("ticks must be multiples of tick_spacing {}", tick_spacing);
    }
    let ta_lower_start = tick_array_start(t_lower, tick_spacing);
    let ta_upper_start = tick_array_start(t_upper, tick_spacing);

    // Mint + NFT ATA
    let position_nft_mint = Keypair::new();
    let position_nft_owner = payer.pubkey();
    let position_nft_account = spl_associated_token_account::get_associated_token_address(
        &position_nft_owner,
        &position_nft_mint.pubkey(),
    );

    // PDAs
    let metadata_account = metadata_pda(&position_nft_mint.pubkey()).0;
    let personal_position = personal_position_pda(&position_nft_mint.pubkey(), &program_id).0;
    let protocol_position = protocol_position_pda(&pool, t_lower, t_upper, &program_id).0;
    let tick_array_lower = tick_array_pda(&pool, ta_lower_start, &program_id).0;
    let tick_array_upper = tick_array_pda(&pool, ta_upper_start, &program_id).0;

    // Determine token programs for mints (Token vs Token-2022)
    let token_program0 = mint_owner_program(&rpc, &mint0).unwrap_or(SPL_TOKEN_PROGRAM_ID);
    let token_program1 = mint_owner_program(&rpc, &mint1).unwrap_or(SPL_TOKEN_PROGRAM_ID);
    let token_program = SPL_TOKEN_PROGRAM_ID;
    let token_program2022 = Pubkey::from_str(SPL_TOKEN_2022_PROGRAM_ID)?;

    // User token accounts (ATA per mint/program)
    let user_token0 = get_associated_token_address_with_program_id(&position_nft_owner, &mint0, &token_program0);
    let user_token1 = get_associated_token_address_with_program_id(&position_nft_owner, &mint1, &token_program1);

    let metas: Vec<AccountMeta> = vec![
        AccountMeta::new(payer.pubkey(), true),
        AccountMeta::new(position_nft_owner, true),
        AccountMeta::new(position_nft_mint.pubkey(), true),
        AccountMeta::new(position_nft_account, false),
        AccountMeta::new(metadata_account, false),
        AccountMeta::new(pool, false),
        AccountMeta::new(protocol_position, false),
        AccountMeta::new(tick_array_lower, false),
        AccountMeta::new(tick_array_upper, false),
        AccountMeta::new(personal_position, false),
        AccountMeta::new(user_token0, false),
        AccountMeta::new(user_token1, false),
        AccountMeta::new(vault0, false),
        AccountMeta::new(vault1, false),
        AccountMeta::new_readonly(sysvar::rent::id(), false),
        AccountMeta::new_readonly(system_program::id(), false),
        AccountMeta::new_readonly(token_program, false),
        AccountMeta::new_readonly(spl_associated_token_account::id(), false),
        AccountMeta::new_readonly(METADATA_PROGRAM_ID, false),
        AccountMeta::new_readonly(token_program2022, false),
        AccountMeta::new_readonly(mint0, false),
        AccountMeta::new_readonly(mint1, false),
    ];

    #[derive(BorshSerialize)]
    struct OpenV2Data {
        tick_lower_index: i32,
        tick_upper_index: i32,
        tick_array_lower_start_index: i32,
        tick_array_upper_start_index: i32,
        liquidity: u128,
        amount0_max: u64,
        amount1_max: u64,
        with_metadata: bool,
        base_flag: Option<bool>,
    }

    let mut data = OpenPositionV2::DISCRIMINATOR.to_vec();
    data.extend_from_slice(&borsh::to_vec(&OpenV2Data {
        tick_lower_index: t_lower,
        tick_upper_index: t_upper,
        tick_array_lower_start_index: ta_lower_start,
        tick_array_upper_start_index: ta_upper_start,
        liquidity: 0,
        amount0_max,
        amount1_max,
        with_metadata: true,
        base_flag: Some(false),
    })?);

    let ix = solana_sdk::instruction::Instruction {
        program_id,
        accounts: metas,
        data,
    };

    let mut tx = Transaction::new_with_payer(&[ix], Some(&payer.pubkey()));
    let blockhash = rpc.get_latest_blockhash()?;
    tx.sign(&[&payer, &position_nft_mint], blockhash);
    let sig = rpc.send_and_confirm_transaction(&tx)?;
    println!("Opened position. Tx: {}", sig);
    println!("Personal position PDA: {}", personal_position);
    println!("Position NFT mint: {}", position_nft_mint.pubkey());
    Ok(())
}

fn metadata_pda(mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[b"metadata", METADATA_PROGRAM_ID.as_ref(), mint.as_ref()],
        &METADATA_PROGRAM_ID,
    )
}

fn personal_position_pda(nft_mint: &Pubkey, program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"personal_position", nft_mint.as_ref()], program_id)
}

fn protocol_position_pda(pool: &Pubkey, tick_lower: i32, tick_upper: i32, program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            b"protocol_position",
            pool.as_ref(),
            &tick_lower.to_le_bytes(),
            &tick_upper.to_le_bytes(),
        ],
        program_id,
    )
}

fn tick_array_pda(pool: &Pubkey, start: i32, program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[b"tick_array", pool.as_ref(), &start.to_le_bytes()],
        program_id,
    )
}

fn mint_owner_program(rpc: &RpcClient, mint: &Pubkey) -> Option<Pubkey> {
    rpc.get_account(mint).ok().map(|acc| acc.owner)
}
