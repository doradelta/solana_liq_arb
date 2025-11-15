use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use carbon_core::deserialize::CarbonDeserialize;
use carbon_raydium_clmm_decoder::accounts::pool_state::PoolState;
use serde::{Deserialize, Serialize};
use solana_client::rpc_client::RpcClient;
use solana_pubkey::Pubkey as RayPubkey;
use solana_sdk::{commitment_config::CommitmentConfig, pubkey::Pubkey};

#[derive(Debug, Serialize, Deserialize)]
pub struct PoolSnapshot {
    pub pool: String,
    pub slot: u64,
    pub amm_config: String,
    #[serde(default)]
    pub program_id: String,
    pub token_mint0: String,
    pub token_mint1: String,
    pub token_vault0: String,
    pub token_vault1: String,
    pub mint_decimals0: u8,
    pub mint_decimals1: u8,
    pub tick_spacing: u16,
    pub data_len: usize,
}

pub fn cache_dir() -> PathBuf {
    PathBuf::from("pool-cache")
}

pub fn cache_file_path(pool: &Pubkey) -> PathBuf {
    cache_dir().join(format!("{pool}.json"))
}

pub fn get_or_fetch_sync(rpc: &RpcClient, pool: &Pubkey, refresh: bool) -> Result<PoolSnapshot> {
    let path = cache_file_path(pool);
    if !refresh && path.exists() {
        let data = fs::read_to_string(&path).context("read cached pool snapshot")?;
        let snap: PoolSnapshot =
            serde_json::from_str(&data).context("parse cached pool snapshot json")?;
        return Ok(snap);
    }

    let snap = fetch_sync(rpc, pool)?;
    write_snapshot(&path, &snap)?;
    Ok(snap)
}

pub fn refresh_sync(rpc: &RpcClient, pool: &Pubkey) -> Result<PoolSnapshot> {
    let snap = fetch_sync(rpc, pool)?;
    let path = cache_file_path(pool);
    write_snapshot(&path, &snap)?;
    Ok(snap)
}

fn fetch_sync(rpc: &RpcClient, pool: &Pubkey) -> Result<PoolSnapshot> {
    let resp = rpc
        .get_account_with_commitment(pool, CommitmentConfig::processed())
        .context("fetch pool account")?;
    let acc = resp
        .value
        .ok_or_else(|| anyhow!("pool account not found"))?;

    let state = <PoolState as CarbonDeserialize>::deserialize(&acc.data[..])
        .ok_or_else(|| anyhow!("decode pool state failed"))?;
    let to_sdk = |p: &RayPubkey| Pubkey::new_from_array(p.to_bytes());

    Ok(PoolSnapshot {
        pool: pool.to_string(),
        slot: resp.context.slot,
        program_id: acc.owner.to_string(),
        amm_config: to_sdk(&state.amm_config).to_string(),
        token_mint0: to_sdk(&state.token_mint0).to_string(),
        token_mint1: to_sdk(&state.token_mint1).to_string(),
        token_vault0: to_sdk(&state.token_vault0).to_string(),
        token_vault1: to_sdk(&state.token_vault1).to_string(),
        mint_decimals0: state.mint_decimals0,
        mint_decimals1: state.mint_decimals1,
        tick_spacing: state.tick_spacing,
        data_len: acc.data.len(),
    })
}

fn write_snapshot(path: &Path, snap: &PoolSnapshot) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("create cache dir")?;
    }
    let json = serde_json::to_string_pretty(snap).context("serialize pool snapshot")?;
    fs::write(path, json).context("write pool snapshot")?;
    Ok(())
}
