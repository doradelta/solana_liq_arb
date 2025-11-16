use std::str::FromStr;

use anyhow::{anyhow, bail, Context, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    compute_budget::ComputeBudgetInstruction,
    instruction::Instruction,
    pubkey::Pubkey,
    signature::{Keypair, SeedDerivable, Signer},
};
use spl_associated_token_account::{
    get_associated_token_address_with_program_id, instruction::create_associated_token_account,
};
use spl_token;
use spl_token_2022;
use solana_pubkey::Pubkey as RawPubkey;
use solana_instruction::Instruction as MetInstruction;

use meteora_sol as met;
use met::accounts::{LbPair, Position};
use met::instructions::{
    add_liquidity::AddLiquidityBuilder,
    initialize_position::InitializePositionBuilder,
    remove_all_liquidity::RemoveAllLiquidityBuilder,
    swap::SwapBuilder,
};
use met::types::{BinLiquidityDistribution, LiquidityParameter};

use crate::cli::Opts;
use crate::tx::{build_unwrap_sol_ix, build_wrap_sol_ixs, simulate_and_send};

pub fn run(opts: Opts) -> Result<()> {
    let rpc_url = opts
        .rpc
        .clone()
        .or_else(|| std::env::var("RPC_URL").ok())
        .unwrap_or_else(|| "https://api.mainnet-beta.solana.com".to_string());
    eprintln!("[debug][meteora] rpc_url={}", rpc_url);
    let rpc = RpcClient::new_with_commitment(rpc_url.clone(), CommitmentConfig::confirmed());

    let key_b58 = std::env::var("PRIVATE_KEY_B58").context("Set PRIVATE_KEY_B58 in .env")?;
    let payer = parse_phantom_base58_key(&key_b58)?;
    let payer_pk = payer.pubkey();

    let pool_opt = opts.pool.clone();

    let mut ixs: Vec<Instruction> = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(opts.cu_limit),
        ComputeBudgetInstruction::set_compute_unit_price(opts.cu_price),
    ];

    if opts.wrap_sol > 0 {
        eprintln!("[debug] wrapping {} lamports into WSOL", opts.wrap_sol);
        ixs.extend(build_wrap_sol_ixs(&rpc, &payer_pk, opts.wrap_sol)?);
    }

    if let Some(pool_str) = &opts.swap_pool {
        handle_swap(&rpc, &payer, &payer_pk, pool_str, &opts, &mut ixs)?;
    } else if let Some(position_str) = &opts.remove_position {
        handle_remove_all(&rpc, &payer, &payer_pk, position_str, &opts, &mut ixs)?;
    } else if let Some(pool_str) = pool_opt.as_ref() {
        handle_open(&rpc, &payer, &payer_pk, pool_str, opts, ixs)?;
        return Ok(());
    }

    if opts.unwrap_sol {
        ixs.push(build_unwrap_sol_ix(&payer_pk));
    }

    if ixs.len() > 2 || opts.unwrap_sol {
        let sig = simulate_and_send(&rpc, &payer, ixs, &[&payer])?;
        println!("✅ Submitted Meteora tx: {}", sig);
    } else {
        if opts.unwrap_sol {
            println!("✅ Unwrapped WSOL.");
        } else {
            bail!("provide swap/open/remove args or wrap/unwrap flags");
        }
    }

    Ok(())
}

fn handle_open(
    rpc: &RpcClient,
    payer: &Keypair,
    payer_pk: &Pubkey,
    pool_str: &str,
    opts: Opts,
    mut ixs: Vec<Instruction>,
) -> Result<()> {
    let lb_pair_pk =
        Pubkey::from_str(pool_str).context("invalid --pool (expected Meteora lb_pair address)")?;
    let req_lower = *opts
        .lower
        .as_ref()
        .context("missing --lower (bin id)")?;
    let req_upper = *opts
        .upper
        .as_ref()
        .context("missing --upper (bin id)")?;
    if req_upper < req_lower {
        bail!("upper must be >= lower (bin ids)");
    }
    if opts.amount0 == 0 && opts.amount1 == 0 {
        bail!("specify --amount0 and/or --amount1");
    }
    let width = (req_upper - req_lower + 1) as i32;

    let lb_acc = rpc
        .get_account(&lb_pair_pk)
        .with_context(|| format!("[meteora::open] fetch lb_pair {}", lb_pair_pk))?;
    let lb_pair: LbPair = LbPair::from_bytes(&lb_acc.data)
        .map_err(|e| anyhow!("[meteora::open] decode LbPair: {e}"))?;

    let token_x_mint = to_sdk_pubkey(&lb_pair.token_x_mint);
    let token_y_mint = to_sdk_pubkey(&lb_pair.token_y_mint);
    let reserve_x = to_sdk_pubkey(&lb_pair.reserve_x);
    let reserve_y = to_sdk_pubkey(&lb_pair.reserve_y);

    let token_x_program = detect_token_program_for_mint(rpc, &token_x_mint)?;
    let token_y_program = detect_token_program_for_mint(rpc, &token_y_mint)?;

    ensure_ata(rpc, &mut ixs, payer_pk, &token_x_mint, &token_x_program)?;
    ensure_ata(rpc, &mut ixs, payer_pk, &token_y_mint, &token_y_program)?;

    let user_token_x =
        get_associated_token_address_with_program_id(payer_pk, &token_x_mint, &token_x_program);
    let user_token_y =
        get_associated_token_address_with_program_id(payer_pk, &token_y_mint, &token_y_program);

    let program_id = sdk_program_id();
    let event_authority = derive_event_authority(&program_id);

    // Derive bin array PDAs for the requested range. If both ends fall into the
    // same BinArray, nudge the upper index so that we pass two distinct accounts
    // to the program (avoids AccountBorrowFailed on duplicate mutable accounts),
    // while still using the original [req_lower, req_upper] for the position.
    let bin_array_lower_index = bin_array_index_for_bin_id(req_lower);
    let mut bin_array_upper_index = bin_array_index_for_bin_id(req_upper);
    if bin_array_lower_index == bin_array_upper_index {
        bin_array_upper_index = bin_array_lower_index + 1;
    }

    let bin_array_lower =
        derive_bin_array_address(&program_id, &lb_pair_pk, bin_array_lower_index);
    let bin_array_upper =
        derive_bin_array_address(&program_id, &lb_pair_pk, bin_array_upper_index);

    let position = Keypair::new();

    let init_ix = InitializePositionBuilder::new()
        .payer(to_raw_pubkey(payer_pk))
        .position(to_raw_pubkey(&position.pubkey()))
        .lb_pair(to_raw_pubkey(&lb_pair_pk))
        .owner(to_raw_pubkey(payer_pk))
        .event_authority(to_raw_pubkey(&event_authority))
        .program(met::LB_CLMM_ID)
        .lower_bin_id(req_lower)
        .width(width)
        .instruction();
    ixs.push(to_sdk_instruction(init_ix));

    let share = uniform_distribution(width as usize, opts.amount0, opts.amount1)?;
    let mut dists = Vec::with_capacity(width as usize);
    for bin_id in req_lower..=req_upper {
        dists.push(BinLiquidityDistribution {
            bin_id,
            distribution_x: if opts.amount0 > 0 { share } else { 0 },
            distribution_y: if opts.amount1 > 0 { share } else { 0 },
        });
    }
    let lp = LiquidityParameter {
        amount_x: opts.amount0,
        amount_y: opts.amount1,
        bin_liquidity_dist: dists,
    };

    let add_ix = AddLiquidityBuilder::new()
        .position(to_raw_pubkey(&position.pubkey()))
        .lb_pair(to_raw_pubkey(&lb_pair_pk))
        .bin_array_bitmap_extension(None)
        .user_token_x(to_raw_pubkey(&user_token_x))
        .user_token_y(to_raw_pubkey(&user_token_y))
        .reserve_x(to_raw_pubkey(&reserve_x))
        .reserve_y(to_raw_pubkey(&reserve_y))
        .token_x_mint(lb_pair.token_x_mint)
        .token_y_mint(lb_pair.token_y_mint)
        .bin_array_lower(to_raw_pubkey(&bin_array_lower))
        .bin_array_upper(to_raw_pubkey(&bin_array_upper))
        .sender(to_raw_pubkey(payer_pk))
        .token_x_program(to_raw_pubkey(&token_x_program))
        .token_y_program(to_raw_pubkey(&token_y_program))
        .event_authority(to_raw_pubkey(&event_authority))
        .program(met::LB_CLMM_ID)
        .liquidity_parameter(lp)
        .instruction();
    ixs.push(to_sdk_instruction(add_ix));

    let sig = simulate_and_send(rpc, payer, ixs, &[payer, &position])?;
    println!(
        "✅ Opened Meteora position. Position account: {}. Tx: {}",
        position.pubkey(),
        sig
    );

    Ok(())
}

fn handle_remove_all(
    rpc: &RpcClient,
    payer: &Keypair,
    payer_pk: &Pubkey,
    position_str: &str,
    opts: &Opts,
    ixs: &mut Vec<Instruction>,
) -> Result<()> {
    let position_pk =
        Pubkey::from_str(position_str).context("invalid --remove-position (Position account)")?;
    let pos_acc = rpc
        .get_account(&position_pk)
        .with_context(|| format!("[meteora::remove] fetch position {}", position_pk))?;
    let pos: Position = Position::from_bytes(&pos_acc.data)
        .map_err(|e| anyhow!("[meteora::remove] decode Position: {e}"))?;

    let lb_pair_pk = to_sdk_pubkey(&pos.lb_pair);
    let lower = pos.lower_bin_id;
    let upper = pos.upper_bin_id;

    let lb_acc = rpc
        .get_account(&lb_pair_pk)
        .with_context(|| format!("[meteora::remove] fetch lb_pair {}", lb_pair_pk))?;
    let lb_pair: LbPair = LbPair::from_bytes(&lb_acc.data)
        .map_err(|e| anyhow!("[meteora::remove] decode LbPair: {e}"))?;

    let token_x_mint = to_sdk_pubkey(&lb_pair.token_x_mint);
    let token_y_mint = to_sdk_pubkey(&lb_pair.token_y_mint);
    let reserve_x = to_sdk_pubkey(&lb_pair.reserve_x);
    let reserve_y = to_sdk_pubkey(&lb_pair.reserve_y);

    let token_x_program = detect_token_program_for_mint(rpc, &token_x_mint)?;
    let token_y_program = detect_token_program_for_mint(rpc, &token_y_mint)?;

    ensure_ata(rpc, ixs, payer_pk, &token_x_mint, &token_x_program)?;
    ensure_ata(rpc, ixs, payer_pk, &token_y_mint, &token_y_program)?;

    let user_token_x =
        get_associated_token_address_with_program_id(payer_pk, &token_x_mint, &token_x_program);
    let user_token_y =
        get_associated_token_address_with_program_id(payer_pk, &token_y_mint, &token_y_program);

    let program_id = sdk_program_id();
    let event_authority = derive_event_authority(&program_id);

    let bin_array_lower_index = bin_array_index_for_bin_id(lower);
    let mut bin_array_upper_index = bin_array_index_for_bin_id(upper);
    if bin_array_lower_index == bin_array_upper_index {
        bin_array_upper_index = bin_array_lower_index + 1;
    }

    let bin_array_lower =
        derive_bin_array_address(&program_id, &lb_pair_pk, bin_array_lower_index);
    let bin_array_upper =
        derive_bin_array_address(&program_id, &lb_pair_pk, bin_array_upper_index);

    let remove_ix = RemoveAllLiquidityBuilder::new()
        .position(to_raw_pubkey(&position_pk))
        .lb_pair(to_raw_pubkey(&lb_pair_pk))
        .bin_array_bitmap_extension(None)
        .user_token_x(to_raw_pubkey(&user_token_x))
        .user_token_y(to_raw_pubkey(&user_token_y))
        .reserve_x(to_raw_pubkey(&reserve_x))
        .reserve_y(to_raw_pubkey(&reserve_y))
        .token_x_mint(lb_pair.token_x_mint)
        .token_y_mint(lb_pair.token_y_mint)
        .bin_array_lower(to_raw_pubkey(&bin_array_lower))
        .bin_array_upper(to_raw_pubkey(&bin_array_upper))
        .sender(to_raw_pubkey(payer_pk))
        .token_x_program(to_raw_pubkey(&token_x_program))
        .token_y_program(to_raw_pubkey(&token_y_program))
        .event_authority(to_raw_pubkey(&event_authority))
        .program(met::LB_CLMM_ID)
        .instruction();
    ixs.push(to_sdk_instruction(remove_ix));

    if opts.close {
        use met::instructions::close_position_if_empty::ClosePositionIfEmptyBuilder;

        let close_ix = ClosePositionIfEmptyBuilder::new()
            .position(to_raw_pubkey(&position_pk))
            .sender(to_raw_pubkey(payer_pk))
            .rent_receiver(to_raw_pubkey(payer_pk))
            .event_authority(to_raw_pubkey(&event_authority))
            .program(met::LB_CLMM_ID)
            .instruction();
        ixs.push(to_sdk_instruction(close_ix));
    }

    Ok(())
}

fn handle_swap(
    rpc: &RpcClient,
    _payer: &Keypair,
    payer_pk: &Pubkey,
    pool_str: &str,
    opts: &Opts,
    ixs: &mut Vec<Instruction>,
) -> Result<()> {
    if opts.swap_amount_in == 0 {
        bail!("--swap-amount-in must be > 0");
    }

    let lb_pair_pk =
        Pubkey::from_str(pool_str).context("invalid --swap-pool (lb_pair address)")?;
    let lb_acc = rpc
        .get_account(&lb_pair_pk)
        .with_context(|| format!("[meteora::swap] fetch lb_pair {}", lb_pair_pk))?;
    let lb_pair: LbPair = LbPair::from_bytes(&lb_acc.data)
        .map_err(|e| anyhow!("[meteora::swap] decode LbPair: {e}"))?;

    let token_x_mint = to_sdk_pubkey(&lb_pair.token_x_mint);
    let token_y_mint = to_sdk_pubkey(&lb_pair.token_y_mint);
    let reserve_x = to_sdk_pubkey(&lb_pair.reserve_x);
    let reserve_y = to_sdk_pubkey(&lb_pair.reserve_y);
    let oracle = to_sdk_pubkey(&lb_pair.oracle);

    let token_x_program = detect_token_program_for_mint(rpc, &token_x_mint)?;
    let token_y_program = detect_token_program_for_mint(rpc, &token_y_mint)?;

    ensure_ata(rpc, ixs, payer_pk, &token_x_mint, &token_x_program)?;
    ensure_ata(rpc, ixs, payer_pk, &token_y_mint, &token_y_program)?;

    let user_token_x =
        get_associated_token_address_with_program_id(payer_pk, &token_x_mint, &token_x_program);
    let user_token_y =
        get_associated_token_address_with_program_id(payer_pk, &token_y_mint, &token_y_program);

    let (user_token_in, user_token_out) = if opts.swap_a_to_b {
        (user_token_x, user_token_y)
    } else {
        (user_token_y, user_token_x)
    };

    let program_id = sdk_program_id();
    let event_authority = derive_event_authority(&program_id);

    // Build a small window of BinArray PDAs around the active bin.
    // DLMM expects these as remaining accounts for swap path traversal.
    let active_id = lb_pair.active_id;
    const BIN_ARRAY_WINDOW: usize = 3;
    let mut indices = Vec::with_capacity(BIN_ARRAY_WINDOW);
    indices.push(bin_array_index_for_bin_id(active_id));
    let mut offset = 1;
    while indices.len() < BIN_ARRAY_WINDOW {
        indices.push(bin_array_index_for_bin_id(active_id + offset * BINS_PER_ARRAY));
        indices.push(bin_array_index_for_bin_id(active_id - offset * BINS_PER_ARRAY));
        offset += 1;
    }

    let mut remaining: Vec<solana_instruction::AccountMeta> =
        Vec::with_capacity(indices.len());
    for idx in indices {
        let ba_sdk = derive_bin_array_address(&program_id, &lb_pair_pk, idx);
        let ba_raw = to_raw_pubkey(&ba_sdk);
        remaining.push(solana_instruction::AccountMeta::new(ba_raw, false));
    }

    let swap_ix = SwapBuilder::new()
        .lb_pair(to_raw_pubkey(&lb_pair_pk))
        .bin_array_bitmap_extension(None)
        .reserve_x(to_raw_pubkey(&reserve_x))
        .reserve_y(to_raw_pubkey(&reserve_y))
        .user_token_in(to_raw_pubkey(&user_token_in))
        .user_token_out(to_raw_pubkey(&user_token_out))
        .token_x_mint(lb_pair.token_x_mint)
        .token_y_mint(lb_pair.token_y_mint)
        .oracle(to_raw_pubkey(&oracle))
        .host_fee_in(None)
        .user(to_raw_pubkey(payer_pk))
        .token_x_program(to_raw_pubkey(&token_x_program))
        .token_y_program(to_raw_pubkey(&token_y_program))
        .event_authority(to_raw_pubkey(&event_authority))
        .program(met::LB_CLMM_ID)
        .amount_in(opts.swap_amount_in)
        .min_amount_out(opts.swap_min_out)
        .add_remaining_accounts(&remaining)
        .instruction();

    ixs.push(to_sdk_instruction(swap_ix));

    Ok(())
}

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

fn to_sdk_instruction(ix: MetInstruction) -> Instruction {
    let MetInstruction {
        program_id,
        accounts,
        data,
    } = ix;
    let program_id_sdk = Pubkey::new_from_array(program_id.to_bytes());
    let accounts_sdk = accounts
        .into_iter()
        .map(|meta| solana_sdk::instruction::AccountMeta {
            pubkey: Pubkey::new_from_array(meta.pubkey.to_bytes()),
            is_signer: meta.is_signer,
            is_writable: meta.is_writable,
        })
        .collect();
    Instruction {
        program_id: program_id_sdk,
        accounts: accounts_sdk,
        data,
    }
}

fn to_raw_pubkey(pk: &Pubkey) -> RawPubkey {
    RawPubkey::new_from_array(pk.to_bytes())
}

fn to_sdk_pubkey(pk: &RawPubkey) -> Pubkey {
    Pubkey::new_from_array(pk.to_bytes())
}

fn sdk_program_id() -> Pubkey {
    Pubkey::new_from_array(met::LB_CLMM_ID.to_bytes())
}

fn derive_event_authority(program_id: &Pubkey) -> Pubkey {
    let (pda, _) = Pubkey::find_program_address(&[b"__event_authority"], program_id);
    pda
}

const BINS_PER_ARRAY: i32 = 70;

fn bin_array_index_for_bin_id(bin_id: i32) -> i64 {
    let per = BINS_PER_ARRAY as i64;
    let id = bin_id as i64;
    if id >= 0 {
        id / per
    } else {
        (id - (per - 1)) / per
    }
}

fn derive_bin_array_address(program_id: &Pubkey, lb_pair: &Pubkey, index: i64) -> Pubkey {
    let mut idx_bytes = [0u8; 8];
    idx_bytes.copy_from_slice(&index.to_le_bytes());
    let (pda, _) =
        Pubkey::find_program_address(&[b"bin_array", lb_pair.as_ref(), &idx_bytes], program_id);
    pda
}

fn uniform_distribution(width: usize, amount_x: u64, amount_y: u64) -> Result<u16> {
    if width == 0 {
        bail!("width must be > 0");
    }
    let base = 10_000u32 / (width as u32);
    let share = if amount_x > 0 || amount_y > 0 {
        base as u16
    } else {
        0
    };
    Ok(share)
}
