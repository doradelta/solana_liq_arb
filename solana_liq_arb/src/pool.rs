use anyhow::{Result, bail};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;

/// Given tick index and tick_spacing, compute the start index of the tick array covering the tick.
pub fn tick_array_start(tick: i32, tick_spacing: i32) -> i32 {
    let span = 60 * tick_spacing;
    // floor division for negatives
    (tick.div_euclid(span)) * span
}

/// A tiny helper to convert price (token1 per token0) to nearest tick index
/// using Uniswap v3 style ticks ~ log base 1.0001.
/// Raydium stores sqrt price, but we just need ticks here.
pub fn price_to_tick(p: f64) -> i32 {
    let ln_1_0001 = 0.000099995; // close enough for selecting a tick
    (p.ln() / ln_1_0001).round() as i32
}

/// Fetch & decode the CLMM pool state (you likely already have the pool id).
/// You’ll also want token0/token1 order and decimals to reason about amounts.
pub async fn fetch_pool(_rpc: &RpcClient, _pool: &Pubkey) -> Result<PoolInfo> {
    // left as an exercise to keep this snippet focused:
    // - fetch account data
    // - decode via carbon-raydium-clmm-decoder accounts::pool_state::PoolState
    // - return tick_spacing, token vaults, current sqrt price etc.
    bail!("implement: fetch & decode pool state")
}

pub struct PoolInfo {
    pub tick_spacing: i32,
    pub token0_mint: Pubkey,
    pub token1_mint: Pubkey,
    // add what you need
}
