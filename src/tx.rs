use anyhow::{Result, bail};
use solana_sdk::{
    commitment_config::CommitmentConfig,
    instruction::Instruction,
    message::Message,
    pubkey::Pubkey,
    signature::{Keypair, Signature, Signer},
    system_instruction,
    transaction::Transaction,
};
use spl_associated_token_account::get_associated_token_address_with_program_id;
use spl_token::{instruction as spl_token_ix, native_mint};

use solana_client::rpc_client::RpcClient;

/// Sign, simulate, and send a transaction.
pub fn simulate_and_send(
    rpc: &RpcClient,
    payer: &Keypair,
    ixs: Vec<Instruction>,
    signers: &[&Keypair],
) -> Result<Signature> {
    let bh = rpc.get_latest_blockhash()?;
    let msg = Message::new(&ixs, Some(&payer.pubkey()));
    let mut tx = Transaction::new_unsigned(msg);
    tx.try_sign(signers, bh)?;
    let sim = rpc.simulate_transaction(&tx)?;
    if let Some(sim_err) = sim.value.err.clone() {
        eprintln!("[debug] simulate_transaction error: {:?}", sim_err);
        if let Some(logs) = sim.value.logs {
            for l in logs {
                eprintln!("[sim log] {}", l);
            }
        }
        bail!("simulation failed: {:?}", sim_err);
    } else if let Some(logs) = sim.value.logs {
        for l in logs {
            eprintln!("[sim log] {}", l);
        }
    }

    let sig: Signature = rpc.send_and_confirm_transaction(&tx)?;
    Ok(sig)
}

/// Build instructions to wrap SOL into WSOL (creates ATA if missing).
pub fn build_wrap_sol_ixs(
    rpc: &RpcClient,
    payer: &Pubkey,
    amount: u64,
) -> Result<Vec<Instruction>> {
    let mut ixs = Vec::new();
    let wsol_mint = native_mint::id();
    let ata = get_associated_token_address_with_program_id(payer, &wsol_mint, &spl_token::ID);
    if rpc
        .get_account_with_commitment(&ata, CommitmentConfig::processed())?
        .value
        .is_none()
    {
        ixs.push(
            spl_associated_token_account::instruction::create_associated_token_account(
                payer,
                payer,
                &wsol_mint,
                &spl_token::ID,
            ),
        );
    }
    ixs.push(system_instruction::transfer(payer, &ata, amount));
    ixs.push(spl_token_ix::sync_native(&spl_token::ID, &ata)?);
    Ok(ixs)
}

/// Build instruction to unwrap WSOL back to SOL (closes ATA).
pub fn build_unwrap_sol_ix(payer: &Pubkey) -> Instruction {
    let wsol_mint = native_mint::id();
    let ata = get_associated_token_address_with_program_id(payer, &wsol_mint, &spl_token::ID);
    spl_token_ix::close_account(&spl_token::ID, &ata, payer, payer, &[]).expect("close_account")
}
