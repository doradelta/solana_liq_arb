use anyhow::{bail, Result};

#[allow(clippy::too_many_arguments)]
pub async fn run_add(
    _rpc_url: &str,
    _payer_path: &str,
    _pool: &str,
    _position: &str,
    _nft_mint: &str,
    _amount0_max: u64,
    _amount1_max: u64,
) -> Result<()> {
    bail!("add-liquidity flow not implemented yet")
}

pub async fn run_remove(
    _rpc_url: &str,
    _payer_path: &str,
    _pool: &str,
    _position: &str,
    _nft_mint: &str,
    _liquidity: u128,
) -> Result<()> {
    bail!("remove-liquidity flow not implemented yet")
}
