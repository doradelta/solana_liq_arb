use std::str::FromStr;

use anchor_lang::{InstructionData, ToAccountMetas};
use anyhow::{Context, Result, anyhow, bail};
use raydium_amm_v3::{accounts as r_accounts, instruction as r_ix, libraries as r_libs};
use raydium_clmm::accounts::{
    personal_position_state::PersonalPositionState as CPersonalPosition,
    pool_state::PoolState as CPoolState,
};
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_request::TokenAccountsFilter;
use solana_pubkey::Pubkey as RawPubkey;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    compute_budget::ComputeBudgetInstruction,
    instruction::{AccountMeta, Instruction},
    program_pack::Pack,
    pubkey::Pubkey,
    signature::{Keypair, SeedDerivable, Signer},
    sysvar,
};
use spl_associated_token_account::{
    ID as ASSOCIATED_TOKEN_PROGRAM_ID, get_associated_token_address_with_program_id,
    instruction::create_associated_token_account,
};
use spl_token::state::Account as SplTokenAccount;
use spl_token_2022::state::Account as SplToken2022Account;

use crate::cli::Opts;
use crate::tx::{build_unwrap_sol_ix, build_wrap_sol_ixs, simulate_and_send};
use mpl_token_metadata::ID as METADATA_PROGRAM_ID;

/// Main entry for CLI dispatch.
pub fn run(opts: Opts) -> Result<()> {
    let rpc_url = opts
        .rpc
        .clone()
        .or_else(|| std::env::var("RPC_URL").ok())
        .unwrap_or_else(|| "https://api.mainnet-beta.solana.com".to_string());
    let rpc = RpcClient::new_with_commitment(rpc_url.clone(), CommitmentConfig::confirmed());

    let key_b58 = std::env::var("PRIVATE_KEY_B58").context("Set PRIVATE_KEY_B58 in .env")?;
    let payer = parse_phantom_base58_key(&key_b58)?;
    let payer_pk = payer.pubkey();

    let clmm_program_id = Pubkey::from_str("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK")?;
    let memo_program_id = Pubkey::from_str("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr")?;

    let mut ixs: Vec<Instruction> = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(opts.cu_limit),
        ComputeBudgetInstruction::set_compute_unit_price(opts.cu_price),
    ];

    if opts.wrap_sol > 0 {
        eprintln!("[debug] wrapping {} lamports into WSOL", opts.wrap_sol);
        ixs.extend(build_wrap_sol_ixs(&rpc, &payer_pk, opts.wrap_sol)?);
    }

    if let Some(pool_str) = &opts.swap_pool {
        handle_swap(
            &rpc,
            &clmm_program_id,
            &payer,
            &payer_pk,
            pool_str,
            &opts,
            &mut ixs,
        )
    } else if let Some(pos_mint_str) = &opts.remove_position {
        handle_remove_all(
            &rpc,
            &clmm_program_id,
            &memo_program_id,
            &payer,
            &payer_pk,
            pos_mint_str,
            &opts,
            &mut ixs,
        )
    } else if opts.pool.is_some() {
        handle_open(&rpc, &clmm_program_id, &payer, &payer_pk, opts, ixs)
    } else {
        if opts.unwrap_sol {
            ixs.push(build_unwrap_sol_ix(&payer_pk));
        }
        if ixs.len() > 2 || opts.unwrap_sol {
            let sig = simulate_and_send(&rpc, &payer, ixs, &[&payer])?;
            println!("✅ Submitted wrap/unwrap tx: {}", sig);
            Ok(())
        } else {
            bail!("provide swap/open/remove args or wrap/unwrap flags");
        }
    }
}

fn parse_phantom_base58_key(s: &str) -> Result<Keypair> {
    let bytes = bs58::decode(s.trim())
        .into_vec()
        .context("Invalid base58 in PRIVATE_KEY_B58")?;
    match bytes.len() {
        64 => Keypair::from_bytes(&bytes).context("Failed to parse 64-byte ed25519 keypair"),
        32 => {
            let seed: [u8; 32] = bytes
                .as_slice()
                .try_into()
                .context("Seed must be 32 bytes")?;
            Keypair::from_seed(&seed)
                .map_err(|e| anyhow!("Failed to derive keypair from 32-byte seed: {e}"))
        }
        n => bail!(
            "Decoded private key had {} bytes; expected 32 or 64 (Phantom exports 64)",
            n
        ),
    }
}

fn decode_pool_clmm(data: &[u8]) -> Result<CPoolState> {
    CPoolState::from_bytes(data).context("decode pool via raydium_clmm")
}

fn decode_personal_position_clmm(data: &[u8]) -> Result<CPersonalPosition> {
    CPersonalPosition::from_bytes(data).context("decode personal position via raydium_clmm")
}

fn to_sdk_pubkey(raw: &RawPubkey) -> Pubkey {
    Pubkey::new_from_array(raw.to_bytes())
}

fn tick_array_start_index(tick: i32, tick_spacing: u16) -> i32 {
    let size = (raydium_amm_v3::states::tick_array::TICK_ARRAY_SIZE as i32) * (tick_spacing as i32);
    let mut start = (tick / size) * size;
    if tick < 0 && tick % size != 0 {
        start -= size;
    }
    start
}

fn derive_tick_array_pda(pool: &Pubkey, start_index: i32, program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            raydium_amm_v3::states::tick_array::TICK_ARRAY_SEED.as_bytes(),
            pool.as_ref(),
            &start_index.to_be_bytes(),
        ],
        program_id,
    )
}

fn derive_personal_position_pda(position_nft_mint: &Pubkey, program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            raydium_amm_v3::states::protocol_position::POSITION_SEED.as_bytes(),
            position_nft_mint.as_ref(),
        ],
        program_id,
    )
}

fn derive_protocol_position_pda(
    pool: &Pubkey,
    lower: i32,
    upper: i32,
    program_id: &Pubkey,
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            raydium_amm_v3::states::protocol_position::POSITION_SEED.as_bytes(),
            pool.as_ref(),
            &lower.to_le_bytes(),
            &upper.to_le_bytes(),
        ],
        program_id,
    )
}

fn find_position_nft_account(
    rpc: &RpcClient,
    owner: &Pubkey,
    mint: &Pubkey,
) -> Result<(Pubkey, Pubkey)> {
    let ata = get_associated_token_address_with_program_id(owner, mint, &spl_token::ID);
    if let Some(acc) = rpc
        .get_account_with_commitment(&ata, CommitmentConfig::processed())?
        .value
    {
        let nft_state =
            SplTokenAccount::unpack_from_slice(&acc.data).context("decode position NFT ATA")?;
        if nft_state.amount > 0 {
            return Ok((ata, acc.owner));
        }
    }

    let token_accounts =
        rpc.get_token_accounts_by_owner(owner, TokenAccountsFilter::Mint(*mint))?;
    for keyed in token_accounts {
        let pk: Pubkey = keyed.pubkey.parse()?;
        let acc = rpc.get_account(&pk)?;
        let amount = if acc.owner == spl_token::ID {
            SplTokenAccount::unpack_from_slice(&acc.data)
                .context("decode position NFT token account")?
                .amount
        } else if acc.owner == spl_token_2022::ID {
            SplToken2022Account::unpack_from_slice(&acc.data)
                .context("decode position NFT token account (2022)")?
                .amount
        } else {
            bail!(
                "position NFT token account uses unsupported token program {}",
                acc.owner
            );
        };
        if amount > 0 {
            return Ok((pk, acc.owner));
        }
    }

    bail!("no token account holding the position NFT was found for the provided signer");
}

fn reward_remaining_accounts(
    rpc: &RpcClient,
    payer: &Pubkey,
    pool: &CPoolState,
    ixs: &mut Vec<Instruction>,
) -> Result<Vec<AccountMeta>> {
    let mut rem: Vec<AccountMeta> = Vec::new();
    for reward in pool.reward_infos.iter() {
        if reward.token_mint == RawPubkey::default() || reward.token_vault == RawPubkey::default() {
            continue;
        }
        let reward_mint = to_sdk_pubkey(&reward.token_mint);
        let reward_vault = to_sdk_pubkey(&reward.token_vault);
        eprintln!(
            "[debug] reward slot: vault={} mint={}",
            reward_vault, reward_mint
        );
        let mint_owner = rpc
            .get_account(&reward_mint)
            .map(|a| a.owner)
            .unwrap_or_else(|e| {
                eprintln!(
                    "[warn] reward mint {} not fetchable ({}); defaulting to SPL Token",
                    reward_mint, e
                );
                spl_token::ID
            });
        let reward_program = if mint_owner == spl_token::ID {
            spl_token::ID
        } else {
            spl_token_2022::ID
        };
        let user_ata =
            get_associated_token_address_with_program_id(payer, &reward_mint, &reward_program);
        if rpc
            .get_account_with_commitment(&user_ata, CommitmentConfig::processed())?
            .value
            .is_none()
        {
            ixs.push(create_associated_token_account(
                payer,
                payer,
                &reward_mint,
                &reward_program,
            ));
        }
        rem.push(AccountMeta::new(reward_vault, false));
        rem.push(AccountMeta::new(user_ata, false));
        rem.push(AccountMeta::new_readonly(reward_mint, false));
    }
    Ok(rem)
}

fn handle_remove_all(
    rpc: &RpcClient,
    clmm_program_id: &Pubkey,
    memo_program_id: &Pubkey,
    payer: &Keypair,
    payer_pk: &Pubkey,
    pos_mint_str: &str,
    opts: &Opts,
    ixs: &mut Vec<Instruction>,
) -> Result<()> {
    let position_mint = Pubkey::from_str(pos_mint_str).context("invalid position NFT mint")?;

    let (personal_position_pda, _) = derive_personal_position_pda(&position_mint, clmm_program_id);
    let personal_acc = rpc
        .get_account(&personal_position_pda)
        .context("fetch personal_position")?;
    if personal_acc.owner != *clmm_program_id {
        bail!("personal_position account owner mismatch (expected Raydium CLMM program)");
    }
    eprintln!(
        "[debug] personal_position len={} lamports={}",
        personal_acc.data.len(),
        personal_acc.lamports
    );
    let personal = decode_personal_position_clmm(&personal_acc.data)?;
    if personal.liquidity == 0 {
        bail!("position has zero liquidity — nothing to remove");
    }
    let pool_id = to_sdk_pubkey(&personal.pool_id);

    let pool_acc = rpc.get_account(&pool_id).context("fetch pool")?;
    if pool_acc.owner != *clmm_program_id {
        bail!("pool account owner mismatch (expected Raydium CLMM program)");
    }
    eprintln!(
        "[debug] pool len={} owner={}",
        pool_acc.data.len(),
        pool_acc.owner
    );
    let pool = decode_pool_clmm(&pool_acc.data)?;
    let token_mint0 = to_sdk_pubkey(&pool.token_mint0);
    let token_mint1 = to_sdk_pubkey(&pool.token_mint1);
    let token_vault0 = to_sdk_pubkey(&pool.token_vault0);
    let token_vault1 = to_sdk_pubkey(&pool.token_vault1);
    eprintln!(
        "[debug] pool tick_spacing={} tick_lo={} tick_hi={} liquidity_in_position={}",
        pool.tick_spacing, personal.tick_lower_index, personal.tick_upper_index, personal.liquidity
    );

    let token_program0 = rpc
        .get_account(&token_mint0)
        .map(|a| a.owner)
        .unwrap_or_else(|e| {
            eprintln!(
                "[warn] mint0 {} not fetchable ({}); defaulting to SPL Token",
                token_mint0, e
            );
            spl_token::ID
        });
    let token_program0 = if token_program0 == spl_token::ID {
        spl_token::ID
    } else {
        spl_token_2022::ID
    };
    let token_program1 = rpc
        .get_account(&token_mint1)
        .map(|a| a.owner)
        .unwrap_or_else(|e| {
            eprintln!(
                "[warn] mint1 {} not fetchable ({}); defaulting to SPL Token",
                token_mint1, e
            );
            spl_token::ID
        });
    let token_program1 = if token_program1 == spl_token::ID {
        spl_token::ID
    } else {
        spl_token_2022::ID
    };

    let ata0 =
        get_associated_token_address_with_program_id(payer_pk, &token_mint0, &token_program0);
    let ata1 =
        get_associated_token_address_with_program_id(payer_pk, &token_mint1, &token_program1);
    if rpc
        .get_account_with_commitment(&ata0, CommitmentConfig::processed())?
        .value
        .is_none()
    {
        ixs.push(create_associated_token_account(
            payer_pk,
            payer_pk,
            &token_mint0,
            &token_program0,
        ));
    }
    if rpc
        .get_account_with_commitment(&ata1, CommitmentConfig::processed())?
        .value
        .is_none()
    {
        ixs.push(create_associated_token_account(
            payer_pk,
            payer_pk,
            &token_mint1,
            &token_program1,
        ));
    }

    let lower = personal.tick_lower_index;
    let upper = personal.tick_upper_index;
    let lower_start = tick_array_start_index(lower, pool.tick_spacing);
    let upper_start = tick_array_start_index(upper, pool.tick_spacing);
    let (tick_array_lower_pda, _) = derive_tick_array_pda(&pool_id, lower_start, clmm_program_id);
    let (tick_array_upper_pda, _) = derive_tick_array_pda(&pool_id, upper_start, clmm_program_id);
    let (protocol_position_pda, _) =
        derive_protocol_position_pda(&pool_id, lower, upper, clmm_program_id);

    let (position_nft_ata, position_nft_program) =
        find_position_nft_account(rpc, payer_pk, &position_mint)?;
    eprintln!("[debug] position NFT account used: {}", position_nft_ata);

    let reward_accounts = reward_remaining_accounts(rpc, payer_pk, &pool, ixs)?;
    eprintln!(
        "[debug] reward groups added: {} ({} accounts)",
        reward_accounts.len() / 3,
        reward_accounts.len()
    );

    let dec_accounts = r_accounts::DecreaseLiquidityV2 {
        nft_owner: *payer_pk,
        nft_account: position_nft_ata,
        personal_position: personal_position_pda,
        pool_state: pool_id,
        protocol_position: protocol_position_pda,
        token_vault_0: token_vault0,
        token_vault_1: token_vault1,
        tick_array_lower: tick_array_lower_pda,
        tick_array_upper: tick_array_upper_pda,
        recipient_token_account_0: ata0,
        recipient_token_account_1: ata1,
        token_program: position_nft_program,
        token_program_2022: spl_token_2022::ID,
        memo_program: *memo_program_id,
        vault_0_mint: token_mint0,
        vault_1_mint: token_mint1,
    };
    let dec_data = r_ix::DecreaseLiquidityV2 {
        liquidity: personal.liquidity,
        amount_0_min: opts.min_out0,
        amount_1_min: opts.min_out1,
    }
    .data();
    let mut dec_metas = dec_accounts.to_account_metas(None);
    dec_metas.extend(reward_accounts);
    ixs.push(Instruction {
        program_id: *clmm_program_id,
        accounts: dec_metas,
        data: dec_data,
    });

    if opts.close {
        let close_accounts = r_accounts::ClosePosition {
            nft_owner: *payer_pk,
            position_nft_mint: position_mint,
            position_nft_account: position_nft_ata,
            personal_position: personal_position_pda,
            system_program: solana_sdk::system_program::id(),
            token_program: position_nft_program,
        };
        let close_ix = Instruction {
            program_id: *clmm_program_id,
            accounts: close_accounts.to_account_metas(None),
            data: r_ix::ClosePosition {}.data(),
        };
        ixs.push(close_ix);
    }

    let sig = simulate_and_send(rpc, payer, ixs.clone(), &[payer])?;
    println!(
        "✅ Removed all liquidity{} for position {}. Tx: {}",
        if opts.close { " and closed" } else { "" },
        position_mint,
        sig
    );

    if opts.unwrap_sol {
        let unwrap_ix = build_unwrap_sol_ix(payer_pk);
        let sig_unwrap = simulate_and_send(rpc, payer, vec![unwrap_ix], &[payer])?;
        println!("✅ Unwrapped WSOL. Tx: {}", sig_unwrap);
    }

    Ok(())
}

fn fetch_token_amount(rpc: &RpcClient, ata: &Pubkey) -> Result<u64> {
    let acc = rpc
        .get_account(ata)
        .with_context(|| format!("fetch token account {}", ata))?;
    if acc.owner == spl_token::ID {
        let state =
            SplTokenAccount::unpack_from_slice(&acc.data).context("decode SPL token account")?;
        return Ok(state.amount);
    }
    if acc.owner == spl_token_2022::ID {
        let state = SplToken2022Account::unpack_from_slice(&acc.data)
            .context("decode SPL token-2022 account")?;
        return Ok(state.amount);
    }
    bail!(
        "token account {} owned by unexpected program {}",
        ata,
        acc.owner
    );
}

fn handle_swap(
    rpc: &RpcClient,
    clmm_program_id: &Pubkey,
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
    let pool_acc = rpc.get_account(&pool_id).context("fetch pool account")?;
    if pool_acc.owner != *clmm_program_id {
        bail!("pool account owner mismatch (expected Raydium CLMM program)");
    }
    let pool = decode_pool_clmm(&pool_acc.data)?;
    let token_mint0 = to_sdk_pubkey(&pool.token_mint0);
    let token_mint1 = to_sdk_pubkey(&pool.token_mint1);
    let token_vault0 = to_sdk_pubkey(&pool.token_vault0);
    let token_vault1 = to_sdk_pubkey(&pool.token_vault1);
    let amm_config = to_sdk_pubkey(&pool.amm_config);
    let observation_state = to_sdk_pubkey(&pool.observation_key);

    let (input_mint, output_mint, input_vault, output_vault) = if opts.swap_a_to_b {
        (token_mint0, token_mint1, token_vault0, token_vault1)
    } else {
        (token_mint1, token_mint0, token_vault1, token_vault0)
    };

    let input_program = rpc
        .get_account(&input_mint)
        .map(|a| a.owner)
        .unwrap_or_else(|e| {
            eprintln!(
                "[warn] input mint {} not fetchable ({}); defaulting to SPL Token",
                input_mint, e
            );
            spl_token::ID
        });
    let output_program = rpc
        .get_account(&output_mint)
        .map(|a| a.owner)
        .unwrap_or_else(|e| {
            eprintln!(
                "[warn] output mint {} not fetchable ({}); defaulting to SPL Token",
                output_mint, e
            );
            spl_token::ID
        });
    if input_program != spl_token::ID || output_program != spl_token::ID {
        bail!(
            "swap_v1 only supports SPL Token mints (no token-2022); input owner {}, output owner {}",
            input_program,
            output_program
        );
    }

    let ata_in =
        get_associated_token_address_with_program_id(payer_pk, &input_mint, &spl_token::ID);
    let ata_out =
        get_associated_token_address_with_program_id(payer_pk, &output_mint, &spl_token::ID);
    if rpc
        .get_account_with_commitment(&ata_in, CommitmentConfig::processed())?
        .value
        .is_none()
    {
        ixs.push(create_associated_token_account(
            payer_pk,
            payer_pk,
            &input_mint,
            &spl_token::ID,
        ));
    }
    if rpc
        .get_account_with_commitment(&ata_out, CommitmentConfig::processed())?
        .value
        .is_none()
    {
        ixs.push(create_associated_token_account(
            payer_pk,
            payer_pk,
            &output_mint,
            &spl_token::ID,
        ));
    }

    let tick_start = tick_array_start_index(pool.tick_current, pool.tick_spacing);
    let (tick_array_pda, _) = derive_tick_array_pda(&pool_id, tick_start, clmm_program_id);

    let accounts = r_accounts::SwapSingle {
        payer: *payer_pk,
        amm_config,
        pool_state: pool_id,
        input_token_account: ata_in,
        output_token_account: ata_out,
        input_vault,
        output_vault,
        observation_state,
        token_program: spl_token::ID,
        tick_array: tick_array_pda,
    };
    let data = r_ix::Swap {
        amount: opts.swap_amount_in,
        other_amount_threshold: opts.swap_min_out,
        sqrt_price_limit_x64: opts.swap_sqrt_price_limit,
        is_base_input: true,
    }
    .data();

    ixs.push(Instruction {
        program_id: *clmm_program_id,
        accounts: accounts.to_account_metas(None),
        data,
    });

    let sig = simulate_and_send(rpc, payer, ixs.clone(), &[payer])?;
    println!(
        "✅ Swap submitted. Tx: {} (amount_in={}, min_out={}, a_to_b={})",
        sig, opts.swap_amount_in, opts.swap_min_out, opts.swap_a_to_b
    );

    if opts.unwrap_sol {
        let unwrap_ix = build_unwrap_sol_ix(payer_pk);
        let sig_unwrap = simulate_and_send(rpc, payer, vec![unwrap_ix], &[payer])?;
        println!("✅ Unwrapped WSOL. Tx: {}", sig_unwrap);
    }

    Ok(())
}

fn handle_open(
    rpc: &RpcClient,
    clmm_program_id: &Pubkey,
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
        bail!("provide at least one non-zero amount (amount0 or amount1)");
    }

    let pool_acc = rpc.get_account(&pool_id).context("fetch pool account")?;
    if pool_acc.owner != *clmm_program_id {
        bail!("pool account owner mismatch (expected Raydium CLMM program) — is this a CLMM pool?");
    }
    eprintln!(
        "[debug] pool data len={} lamports={} owner={}",
        pool_acc.data.len(),
        pool_acc.lamports,
        pool_acc.owner
    );
    let pool = decode_pool_clmm(&pool_acc.data)?;
    let token_mint0 = to_sdk_pubkey(&pool.token_mint0);
    let token_mint1 = to_sdk_pubkey(&pool.token_mint1);
    let token_vault0 = to_sdk_pubkey(&pool.token_vault0);
    let token_vault1 = to_sdk_pubkey(&pool.token_vault1);

    let tick_spacing = pool.tick_spacing as i32;
    if lower % tick_spacing != 0 || upper % tick_spacing != 0 {
        bail!(
            "ticks must be multiples of pool.tick_spacing = {}",
            tick_spacing
        );
    }

    let token_program0 = rpc
        .get_account(&token_mint0)
        .map(|a| a.owner)
        .unwrap_or_else(|e| {
            eprintln!(
                "[warn] mint0 {} not fetchable ({}); defaulting to SPL Token",
                token_mint0, e
            );
            spl_token::ID
        });
    let token_program0 = if token_program0 == spl_token::ID {
        spl_token::ID
    } else {
        spl_token_2022::ID
    };
    let token_program1 = rpc
        .get_account(&token_mint1)
        .map(|a| a.owner)
        .unwrap_or_else(|e| {
            eprintln!(
                "[warn] mint1 {} not fetchable ({}); defaulting to SPL Token",
                token_mint1, e
            );
            spl_token::ID
        });
    let token_program1 = if token_program1 == spl_token::ID {
        spl_token::ID
    } else {
        spl_token_2022::ID
    };

    let ata0 =
        get_associated_token_address_with_program_id(payer_pk, &token_mint0, &token_program0);
    let ata1 =
        get_associated_token_address_with_program_id(payer_pk, &token_mint1, &token_program1);

    if rpc
        .get_account_with_commitment(&ata0, CommitmentConfig::processed())?
        .value
        .is_none()
    {
        ixs.push(create_associated_token_account(
            payer_pk,
            payer_pk,
            &token_mint0,
            &token_program0,
        ));
    }
    if rpc
        .get_account_with_commitment(&ata1, CommitmentConfig::processed())?
        .value
        .is_none()
    {
        ixs.push(create_associated_token_account(
            payer_pk,
            payer_pk,
            &token_mint1,
            &token_program1,
        ));
    }

    let bal0 = fetch_token_amount(rpc, &ata0).unwrap_or(0);
    let bal1 = fetch_token_amount(rpc, &ata1).unwrap_or(0);
    eprintln!(
        "[debug] user balances before open: token0 {} ({}), token1 {} ({})",
        token_mint0, bal0, token_mint1, bal1
    );

    let position_mint = Keypair::new();
    let (metadata_pda, _bump) =
        mpl_token_metadata::pda::find_metadata_account(&position_mint.pubkey());
    let position_nft_ata = get_associated_token_address_with_program_id(
        payer_pk,
        &position_mint.pubkey(),
        &spl_token::ID,
    );

    let lower_start = tick_array_start_index(lower, pool.tick_spacing);
    let upper_start = tick_array_start_index(upper, pool.tick_spacing);
    let (tick_array_lower_pda, _) = derive_tick_array_pda(&pool_id, lower_start, clmm_program_id);
    let (tick_array_upper_pda, _) = derive_tick_array_pda(&pool_id, upper_start, clmm_program_id);
    let (personal_position_pda, _) =
        derive_personal_position_pda(&position_mint.pubkey(), clmm_program_id);
    let (protocol_position_pda, _) =
        derive_protocol_position_pda(&pool_id, lower, upper, clmm_program_id);

    let sqrt_ratio_x64 = pool.sqrt_price_x64;
    let sqrt_a_x64 =
        r_libs::tick_math::get_sqrt_price_at_tick(lower).context("sqrt_at_tick lower")?;
    let sqrt_b_x64 =
        r_libs::tick_math::get_sqrt_price_at_tick(upper).context("sqrt_at_tick upper")?;
    let (sqrt_lo, sqrt_hi) = if sqrt_a_x64 < sqrt_b_x64 {
        (sqrt_a_x64, sqrt_b_x64)
    } else {
        (sqrt_b_x64, sqrt_a_x64)
    };

    let liquidity = if opts.amount0 > 0 && opts.amount1 == 0 {
        if sqrt_ratio_x64 >= sqrt_hi {
            bail!(
                "Your current price is ABOVE the range; token0-only cannot open here (range needs token1). Choose a higher range or provide token1."
            );
        }
        r_libs::liquidity_math::get_liquidity_from_single_amount_0(
            sqrt_ratio_x64,
            sqrt_lo,
            sqrt_hi,
            opts.amount0,
        )
    } else if opts.amount1 > 0 && opts.amount0 == 0 {
        if sqrt_ratio_x64 <= sqrt_lo {
            bail!(
                "Your current price is BELOW the range; token1-only cannot open here (range needs token0). Choose a lower range or provide token0."
            );
        }
        r_libs::liquidity_math::get_liquidity_from_single_amount_1(
            sqrt_ratio_x64,
            sqrt_lo,
            sqrt_hi,
            opts.amount1,
        )
    } else {
        r_libs::liquidity_math::get_liquidity_from_amounts(
            sqrt_ratio_x64,
            sqrt_lo,
            sqrt_hi,
            opts.amount0,
            opts.amount1,
        )
    };

    if liquidity == 0 {
        bail!(
            "computed liquidity is zero — adjust amounts or pick a range closer to the current price"
        );
    }

    let accounts = r_accounts::OpenPositionV2 {
        payer: *payer_pk,
        position_nft_owner: *payer_pk,
        position_nft_mint: position_mint.pubkey(),
        position_nft_account: position_nft_ata,
        metadata_account: metadata_pda,
        pool_state: pool_id,
        protocol_position: protocol_position_pda,
        tick_array_lower: tick_array_lower_pda,
        tick_array_upper: tick_array_upper_pda,
        personal_position: personal_position_pda,
        token_account_0: ata0,
        token_account_1: ata1,
        token_vault_0: token_vault0,
        token_vault_1: token_vault1,
        rent: sysvar::rent::id(),
        system_program: solana_sdk::system_program::id(),
        token_program: spl_token::ID,
        associated_token_program: ASSOCIATED_TOKEN_PROGRAM_ID,
        metadata_program: METADATA_PROGRAM_ID,
        token_program_2022: spl_token_2022::ID,
        vault_0_mint: token_mint0,
        vault_1_mint: token_mint1,
    };

    let data = r_ix::OpenPositionV2 {
        tick_lower_index: lower,
        tick_upper_index: upper,
        tick_array_lower_start_index: lower_start,
        tick_array_upper_start_index: upper_start,
        liquidity,
        amount_0_max: opts.amount0,
        amount_1_max: opts.amount1,
        with_matedata: true,
        base_flag: None,
    }
    .data();

    let ix = Instruction {
        program_id: *clmm_program_id,
        accounts: accounts.to_account_metas(None),
        data,
    };
    ixs.push(ix);

    let sig = simulate_and_send(rpc, payer, ixs.clone(), &[payer, &position_mint])?;
    println!("✅ Submitted. Tx: {}", sig);

    if opts.unwrap_sol {
        let unwrap_ix = build_unwrap_sol_ix(payer_pk);
        let sig_unwrap = simulate_and_send(rpc, payer, vec![unwrap_ix], &[payer])?;
        println!("✅ Unwrapped WSOL. Tx: {}", sig_unwrap);
    }

    Ok(())
}
