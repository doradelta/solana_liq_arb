
use std::str::FromStr;

use anyhow::{anyhow, bail, Context, Result};
use borsh::BorshDeserialize;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    compute_budget::ComputeBudgetInstruction,
    instruction::Instruction,
    pubkey::Pubkey,
    signature::{Keypair, SeedDerivable, Signer},
    system_program,
};
use spl_associated_token_account::{
    get_associated_token_address_with_program_id, instruction::create_associated_token_account,
};
use orca_whirlpools_client as owc; // low-level (IDL-generated) client crate
use owc::{
    Whirlpool,
    Position,
    SwapV2,
    SwapV2InstructionArgs,
    OpenPosition,
    OpenPositionInstructionArgs,
    IncreaseLiquidityV2,
    IncreaseLiquidityV2InstructionArgs,
    DecreaseLiquidityV2,
    DecreaseLiquidityV2InstructionArgs,
    CollectFeesV2,
    CollectFeesV2InstructionArgs,
    ClosePosition,
    get_oracle_address,
    get_tick_array_address,
    get_position_address,
};

use orca_whirlpools_core as ocore; // math / quoting utilities
use ocore::{get_tick_array_start_tick_index, MAX_SQRT_PRICE, MIN_SQRT_PRICE, TICK_ARRAY_SIZE};

use crate::cli::Opts;
use crate::tx::{build_unwrap_sol_ix, build_wrap_sol_ixs, simulate_and_send};

const MEMO_PROGRAM_ID: &str = "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr";

pub fn run(opts: Opts) -> Result<()> {
    let rpc_url = opts
        .rpc
        .clone()
        .or_else(|| std::env::var("RPC_URL").ok())
        .unwrap_or_else(|| "https://api.mainnet-beta.solana.com".to_string());
    eprintln!("[debug][orca] rpc_url={}", rpc_url);
    let rpc = RpcClient::new_with_commitment(rpc_url.clone(), CommitmentConfig::confirmed());

    let key_b58 = std::env::var("PRIVATE_KEY_B58").context("Set PRIVATE_KEY_B58 in .env")?;
    let payer = parse_phantom_base58_key(&key_b58)?;
    let payer_pk = payer.pubkey();

    // Mainnet Orca Whirlpools program id (constant).
    let whirlpool_program_id = Pubkey::from_str("whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc")?;
    eprintln!("[debug][orca] whirlpool_program_id={}", whirlpool_program_id);

    let memo_program_id = Pubkey::from_str(MEMO_PROGRAM_ID)?;

    let mut ixs: Vec<Instruction> = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(opts.cu_limit),
        ComputeBudgetInstruction::set_compute_unit_price(opts.cu_price),
    ];

    if opts.wrap_sol > 0 {
        eprintln!("[debug] wrapping {} lamports into WSOL", opts.wrap_sol);
        ixs.extend(build_wrap_sol_ixs(&rpc, &payer_pk, opts.wrap_sol)?);
    }

    // Mirror the Raydium flow selection:
    // - swap if --swap-pool is provided,
    // - remove if --remove-position is provided,
    // - else open if --pool is provided.
    if let Some(pool_str) = &opts.swap_pool {
        handle_swap(&rpc, &whirlpool_program_id, &payer, &payer_pk, pool_str, &opts, &mut ixs)?;
    } else if let Some(pos_mint_str) = &opts.remove_position {
        handle_remove_all(
            &rpc,
            &whirlpool_program_id,
            &memo_program_id,
            &payer,
            &payer_pk,
            pos_mint_str,
            &opts,
            &mut ixs,
        )?;
    } else if opts.pool.is_some() {
        handle_open(&rpc, &whirlpool_program_id, &payer, &payer_pk, opts, ixs)?;
        // handle_open internally sends the transaction (like Raydium's version).
        return Ok(());
    }

    if opts.unwrap_sol {
        let unwrap_ix = build_unwrap_sol_ix(&payer_pk);
        // send any pending ixs + unwrap in a single tx for convenience
        ixs.push(unwrap_ix);
    }

    if ixs.len() > 2 {
        let sig = simulate_and_send(&rpc, &payer, ixs, &[&payer])?;
        println!("✅ Submitted. Tx: {}", sig);
    } else {
        // Only compute budget ixs were configured and nothing else to do
        if opts.unwrap_sol {
            println!("✅ Unwrapped WSOL.");
        }
    }

    Ok(())
}

// ----------------------------- Swap -----------------------------

fn handle_swap(
    rpc: &RpcClient,
    program_id: &Pubkey,
    payer: &Keypair,
    payer_pk: &Pubkey,
    pool_str: &str,
    opts: &Opts,
    ixs: &mut Vec<Instruction>,
) -> Result<()> {
    if opts.swap_amount_in == 0 {
        bail!("--swap-amount-in must be > 0");
    }
    let pool_id = Pubkey::from_str(pool_str).context("invalid swap pool id")?;
    let pool_acc = rpc
        .get_account(&pool_id)
        .with_context(|| format!("[orca::swap] fetch whirlpool account {}", pool_id))?;
    eprintln!(
        "[debug][orca::swap] whirlpool={} owner={} data_len={}",
        pool_id,
        pool_acc.owner,
        pool_acc.data.len()
    );
    if pool_acc.owner != *program_id {
        bail!("pool account owner mismatch (expected Orca Whirlpool program)");
    }

    let whirl: Whirlpool = decode_whirlpool(&pool_acc.data).with_context(|| {
        format!(
            "[orca::swap] decode whirlpool {} (data_len={})",
            pool_id,
            pool_acc.data.len()
        )
    })?;
    let oracle = get_oracle_address(&pool_id)?.0;

    // Decide swap direction & which side is input/output.
    let a_to_b = opts.swap_a_to_b;

    // Determine token programs per mint (handles Token-2022 automatically).
    let token_program_a = detect_token_program_for_mint(rpc, &whirl.token_mint_a)?;
    let token_program_b = detect_token_program_for_mint(rpc, &whirl.token_mint_b)?;

    // Ensure owner ATAs exist for both mints
    let ata_a = get_associated_token_address_with_program_id(payer_pk, &whirl.token_mint_a, &token_program_a);
    let ata_b = get_associated_token_address_with_program_id(payer_pk, &whirl.token_mint_b, &token_program_b);
    ensure_ata(rpc, ixs, payer_pk, &whirl.token_mint_a, &token_program_a)?;
    ensure_ata(rpc, ixs, payer_pk, &whirl.token_mint_b, &token_program_b)?;

    // Tick arrays: take current array and two neighbors in the swap direction (standard pattern).
    let current_tick = whirl.tick_current_index;
    let tick_spacing = whirl.tick_spacing;
    let ts_i32 = tick_spacing as i32;
    let arr_span = ts_i32 * TICK_ARRAY_SIZE as i32;
    let start0 = get_tick_array_start_tick_index(current_tick, tick_spacing);
    let (start1, start2) = if a_to_b {
        (start0 - arr_span, start0 - 2 * arr_span)
    } else {
        (start0 + arr_span, start0 + 2 * arr_span)
    };

    let (tick_array0, _) = get_tick_array_address(&pool_id, start0)?;
    let (tick_array1, _) = get_tick_array_address(&pool_id, start1)?;
    let (tick_array2, _) = get_tick_array_address(&pool_id, start2)?;

    // Build SwapV2 instruction.
    let sqrt_price_limit = if opts.swap_sqrt_price_limit == 0 {
        if a_to_b { MIN_SQRT_PRICE } else { MAX_SQRT_PRICE }
    } else {
        opts.swap_sqrt_price_limit
    };

    let args = SwapV2InstructionArgs {
        amount: opts.swap_amount_in,
        other_amount_threshold: opts.swap_min_out,
        sqrt_price_limit,
        amount_specified_is_input: true,
        a_to_b,
        remaining_accounts_info: None,
    };

    let swap_accounts = SwapV2 {
        token_program_a: token_program_a,
        token_program_b: token_program_b,
        memo_program: Pubkey::from_str(MEMO_PROGRAM_ID)?,
        token_authority: *payer_pk,
        whirlpool: pool_id,
        token_mint_a: whirl.token_mint_a,
        token_mint_b: whirl.token_mint_b,
        token_owner_account_a: ata_a,
        token_vault_a: whirl.token_vault_a,
        token_owner_account_b: ata_b,
        token_vault_b: whirl.token_vault_b,
        tick_array0,
        tick_array1,
        tick_array2,
        oracle,
    };
    let swap_ix = swap_accounts.instruction(args);
    ixs.push(swap_ix);

    Ok(())
}

// ----------------------------- Open Position -----------------------------

fn handle_open(
    rpc: &RpcClient,
    program_id: &Pubkey,
    payer: &Keypair,
    payer_pk: &Pubkey,
    opts: Opts,
    mut ixs: Vec<Instruction>,
) -> Result<()> {
    let pool_id = Pubkey::from_str(opts.pool.as_ref().context("missing --pool")?)
        .context("invalid pool id")?;
    let lower = *opts.lower.as_ref().context("missing --lower")?;
    let upper = *opts.upper.as_ref().context("missing --upper")?;
    if upper <= lower {
        bail!("upper tick must be > lower tick");
    }
    if opts.amount0 == 0 && opts.amount1 == 0 {
        bail!("specify --amount0 and/or --amount1");
    }

    let pool_acc = rpc
        .get_account(&pool_id)
        .with_context(|| format!("[orca::open] fetch whirlpool {}", pool_id))?;
    eprintln!(
        "[debug][orca::open] whirlpool={} owner={} data_len={}",
        pool_id,
        pool_acc.owner,
        pool_acc.data.len()
    );
    if pool_acc.owner != *program_id {
        bail!("pool account owner mismatch (expected Orca Whirlpool program)");
    }
    let whirl: Whirlpool = decode_whirlpool(&pool_acc.data).with_context(|| {
        format!(
            "[orca::open] decode whirlpool {} (data_len={})",
            pool_id,
            pool_acc.data.len()
        )
    })?;

    // Ensure owner ATAs for both mints
    let token_program_a = detect_token_program_for_mint(rpc, &whirl.token_mint_a)?;
    let token_program_b = detect_token_program_for_mint(rpc, &whirl.token_mint_b)?;
    let ata_a = get_associated_token_address_with_program_id(payer_pk, &whirl.token_mint_a, &token_program_a);
    let ata_b = get_associated_token_address_with_program_id(payer_pk, &whirl.token_mint_b, &token_program_b);
    ensure_ata(rpc, &mut ixs, payer_pk, &whirl.token_mint_a, &token_program_a)?;
    ensure_ata(rpc, &mut ixs, payer_pk, &whirl.token_mint_b, &token_program_b)?;

    // Derive tick-array PDAs for the provided ticks
    let tick_spacing = whirl.tick_spacing;
    let lower_start = get_tick_array_start_tick_index(lower, tick_spacing);
    let upper_start = get_tick_array_start_tick_index(upper, tick_spacing);
    let (tick_array_lower, _) = get_tick_array_address(&pool_id, lower_start)?;
    let (tick_array_upper, _) = get_tick_array_address(&pool_id, upper_start)?;

    // Create a fresh position NFT mint & ATA
    let position_mint = Keypair::new();
    let (position_pda, position_bump) = get_position_address(&position_mint.pubkey())?;
    let position_token_account = get_associated_token_address_with_program_id(
        payer_pk,
        &position_mint.pubkey(),
        &spl_token::ID,
    );

    // OpenPosition (no metadata to keep dependencies light)
    let open_ix = OpenPosition {
        funder: *payer_pk,
        owner: *payer_pk,
        position: position_pda,
        position_mint: position_mint.pubkey(),
        position_token_account,
        whirlpool: pool_id,
        token_program: spl_token::ID,
        system_program: system_program::id(),
        rent: solana_sdk::sysvar::rent::id(),
        associated_token_program: spl_associated_token_account::id(),
    }
    .instruction(OpenPositionInstructionArgs {
        position_bump,
        tick_lower_index: lower,
        tick_upper_index: upper,
    });
    ixs.push(open_ix);

    // Quote liquidity for the provided token amounts and current sqrt price.
    let sqrt_price_x64 = whirl.sqrt_price; // u128
    let slippage_bps: u16 = 0;
    let liq_quote = if opts.amount0 > 0 && opts.amount1 == 0 {
        ocore::increase_liquidity_quote_a(
            opts.amount0,
            slippage_bps,
            sqrt_price_x64,
            lower,
            upper,
            None,
            None,
        )
        .map_err(|e| anyhow!("liquidity quote failed (token0 only): {:?}", e))?
    } else if opts.amount1 > 0 && opts.amount0 == 0 {
        ocore::increase_liquidity_quote_b(
            opts.amount1,
            slippage_bps,
            sqrt_price_x64,
            lower,
            upper,
            None,
            None,
        )
        .map_err(|e| anyhow!("liquidity quote failed (token1 only): {:?}", e))?
    } else {
        // Both token0 and token1 provided: try token0-driven quote first, then token1-driven.
        let quote_a = ocore::increase_liquidity_quote_a(
            opts.amount0,
            slippage_bps,
            sqrt_price_x64,
            lower,
            upper,
            None,
            None,
        )
        .map_err(|e| anyhow!("liquidity quote failed (token0): {:?}", e))?;
        if quote_a.token_max_b <= opts.amount1 {
            quote_a
        } else {
            let quote_b = ocore::increase_liquidity_quote_b(
                opts.amount1,
                slippage_bps,
                sqrt_price_x64,
                lower,
                upper,
                None,
                None,
            )
            .map_err(|e| anyhow!("liquidity quote failed (token1): {:?}", e))?;
            if quote_b.token_max_a <= opts.amount0 {
                quote_b
            } else {
                bail!(
                    "provided token amounts are too low for both sides at current price (need up to token_max_a={}, token_max_b={})",
                    quote_b.token_max_a,
                    quote_a.token_max_b,
                );
            }
        }
    };

    // IncreaseLiquidityV2
    let inc_ix = IncreaseLiquidityV2 {
        whirlpool: pool_id,
        token_program_a: token_program_a,
        token_program_b: token_program_b,
        memo_program: Pubkey::from_str(MEMO_PROGRAM_ID)?,
        position_authority: *payer_pk,
        position: position_pda,
        position_token_account,
        token_mint_a: whirl.token_mint_a,
        token_mint_b: whirl.token_mint_b,
        token_owner_account_a: ata_a,
        token_owner_account_b: ata_b,
        token_vault_a: whirl.token_vault_a,
        token_vault_b: whirl.token_vault_b,
        tick_array_lower,
        tick_array_upper,
    }
    .instruction(IncreaseLiquidityV2InstructionArgs {
        liquidity_amount: liq_quote.liquidity_delta,
        token_max_a: liq_quote.token_max_a,
        token_max_b: liq_quote.token_max_b,
        remaining_accounts_info: None,
    });
    ixs.push(inc_ix);

    // Send the tx that does: (compute budget) + create ATAs + open + increase
    let sig = simulate_and_send(rpc, payer, ixs, &[payer, &position_mint])?;
    println!("✅ Opened Orca position. Position mint: {}. Tx: {}", position_mint.pubkey(), sig);
    Ok(())
}

// ----------------------------- Remove / Close Position -----------------------------

fn handle_remove_all(
    rpc: &RpcClient,
    program_id: &Pubkey,
    memo_program_id: &Pubkey,
    payer: &Keypair,
    payer_pk: &Pubkey,
    pos_mint_str: &str,
    _opts: &Opts,
    ixs: &mut Vec<Instruction>,
) -> Result<()> {
    let position_mint = Pubkey::from_str(pos_mint_str).context("invalid position NFT mint")?;
    let (position_pda, _) = get_position_address(&position_mint)?;
    let pos_acc = rpc
        .get_account(&position_pda)
        .with_context(|| format!("[orca::remove] fetch position account {}", position_pda))?;
    eprintln!(
        "[debug][orca::remove] position_pda={} data_len={}",
        position_pda,
        pos_acc.data.len()
    );

    // Tightly-coupled borsh decode (IDL-generated struct layout). Discriminator is present; skip 8 bytes.
    let position: Position = decode_position(&pos_acc.data).with_context(|| {
        format!(
            "[orca::remove] decode position {} (data_len={})",
            position_pda,
            pos_acc.data.len()
        )
    })?;

    let pool_id = position.whirlpool;
    let pool_acc = rpc
        .get_account(&pool_id)
        .with_context(|| format!("[orca::remove] fetch whirlpool {}", pool_id))?;
    eprintln!(
        "[debug][orca::remove] whirlpool={} owner={} data_len={}",
        pool_id,
        pool_acc.owner,
        pool_acc.data.len()
    );
    if pool_acc.owner != *program_id {
        bail!("position's whirlpool not owned by Orca program");
    }
    let whirl: Whirlpool = decode_whirlpool(&pool_acc.data).with_context(|| {
        format!(
            "[orca::remove] decode whirlpool {} (data_len={})",
            pool_id,
            pool_acc.data.len()
        )
    })?;

    let token_program_a = detect_token_program_for_mint(rpc, &whirl.token_mint_a)?;
    let token_program_b = detect_token_program_for_mint(rpc, &whirl.token_mint_b)?;

    let ata_a = get_associated_token_address_with_program_id(payer_pk, &whirl.token_mint_a, &token_program_a);
    let ata_b = get_associated_token_address_with_program_id(payer_pk, &whirl.token_mint_b, &token_program_b);
    ensure_ata(rpc, ixs, payer_pk, &whirl.token_mint_a, &token_program_a)?;
    ensure_ata(rpc, ixs, payer_pk, &whirl.token_mint_b, &token_program_b)?;

    let tick_spacing = whirl.tick_spacing;
    let lower_start = get_tick_array_start_tick_index(position.tick_lower_index, tick_spacing);
    let upper_start = get_tick_array_start_tick_index(position.tick_upper_index, tick_spacing);
    let (tick_array_lower, _) = get_tick_array_address(&pool_id, lower_start)?;
    let (tick_array_upper, _) = get_tick_array_address(&pool_id, upper_start)?;

    // If there is any liquidity, remove it.
    if position.liquidity > 0 {
        let dec_ix = DecreaseLiquidityV2 {
            whirlpool: pool_id,
            token_program_a,
            token_program_b,
            memo_program: *memo_program_id,
            position_authority: *payer_pk,
            position: position_pda,
            position_token_account: get_associated_token_address_with_program_id(
                payer_pk,
                &position_mint,
                &spl_token::ID,
            ),
            token_mint_a: whirl.token_mint_a,
            token_mint_b: whirl.token_mint_b,
            token_owner_account_a: ata_a,
            token_owner_account_b: ata_b,
            token_vault_a: whirl.token_vault_a,
            token_vault_b: whirl.token_vault_b,
            tick_array_lower,
            tick_array_upper,
        }
        .instruction(DecreaseLiquidityV2InstructionArgs {
            liquidity_amount: position.liquidity,
            token_min_a: 0,
            token_min_b: 0,
            remaining_accounts_info: None,
        });
        ixs.push(dec_ix);

        // Then collect any fees owed to the position into owner ATAs.
        let collect_ix = CollectFeesV2 {
            whirlpool: pool_id,
            position_authority: *payer_pk,
            position: position_pda,
            position_token_account: get_associated_token_address_with_program_id(
                payer_pk,
                &position_mint,
                &spl_token::ID,
            ),
            token_mint_a: whirl.token_mint_a,
            token_mint_b: whirl.token_mint_b,
            token_owner_account_a: ata_a,
            token_vault_a: whirl.token_vault_a,
            token_owner_account_b: ata_b,
            token_vault_b: whirl.token_vault_b,
            token_program_a,
            token_program_b,
            memo_program: *memo_program_id,
        }
        .instruction(CollectFeesV2InstructionArgs {
            remaining_accounts_info: None,
        });
        ixs.push(collect_ix);
    }

    // Finally, close the position and burn the NFT from the owner's token account.
    let close_ix = ClosePosition {
        position_authority: *payer_pk,
        receiver: *payer_pk,
        position: position_pda,
        position_mint,
        position_token_account: get_associated_token_address_with_program_id(payer_pk, &position_mint, &spl_token::ID),
        token_program: spl_token::ID,
    }
    .instruction();
    ixs.push(close_ix);

    Ok(())
}

// ----------------------------- Helpers -----------------------------

fn parse_phantom_base58_key(s: &str) -> Result<Keypair> {
    let bytes = bs58::decode(s.trim())
        .into_vec()
        .context("Invalid base58 in PRIVATE_KEY_B58")?;
    match bytes.len() {
        64 => Keypair::from_bytes(&bytes).context("Failed to parse 64-byte ed25519 keypair"),
        32 => {
            let mut seed = [0u8; 32];
            seed.copy_from_slice(&bytes);
            Keypair::from_seed(&seed)
                .map_err(|e| anyhow!("Failed to derive keypair from 32-byte seed: {e}"))
        }
        n => bail!(
            "Decoded private key had {} bytes; expected 32 or 64 (Phantom exports 64)",
            n
        ),
    }
}

fn ensure_ata(
    rpc: &RpcClient,
    ixs: &mut Vec<Instruction>,
    owner: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
) -> Result<()> {
    let ata = get_associated_token_address_with_program_id(owner, mint, token_program);
    if rpc
        .get_account_with_commitment(&ata, CommitmentConfig::processed())?
        .value
        .is_none()
    {
        ixs.push(create_associated_token_account(
            owner, owner, mint, token_program,
        ));
    }
    Ok(())
}

fn detect_token_program_for_mint(rpc: &RpcClient, mint: &Pubkey) -> Result<Pubkey> {
    let acc = rpc.get_account(mint)?;
    if acc.owner == spl_token_2022::ID {
        Ok(spl_token_2022::ID)
    } else {
        Ok(spl_token::ID)
    }
}

// Anchor-like account decoders (skip the 8-byte discriminator)
fn decode_whirlpool(data: &[u8]) -> Result<Whirlpool> {
    if data.len() != Whirlpool::LEN {
        bail!(
            "whirlpool account length mismatch: got {}, expected {}",
            data.len(),
            Whirlpool::LEN
        );
    }
    let mut slice = data;
    Whirlpool::deserialize(&mut slice)
        .with_context(|| format!("decode Whirlpool account from buffer (len={})", data.len()))
}

fn decode_position(data: &[u8]) -> Result<Position> {
    if data.len() != Position::LEN {
        bail!(
            "position account length mismatch: got {}, expected {}",
            data.len(),
            Position::LEN
        );
    }
    let mut slice = data;
    Position::deserialize(&mut slice)
        .with_context(|| format!("decode Position account from buffer (len={})", data.len()))
}
