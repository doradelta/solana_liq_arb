use clap::{Parser, ValueEnum};

/// Mainnet helper for Raydium, Orca & Meteora CLMM/DLMM and WSOL utilities.
#[derive(Parser, Debug)]
#[command(
    version,
    about = "CLMM/DLMM helper for Raydium, Orca & Meteora (open/remove position, swap, wrap/unwrap SOL)."
)]
pub struct Opts {
    /// Which DEX to target (raydium|orca|meteora). Default: raydium.
    #[arg(long, value_enum, default_value_t = Dex::Raydium)]
    pub dex: Dex,

    /// Optional mainnet RPC URL (defaults to env RPC_URL or public mainnet RPC)
    #[arg(long)]
    pub rpc: Option<String>,

    /// Optional: microlamports per CU for priority fees (default 1000)
    #[arg(long, default_value_t = 1000)]
    pub cu_price: u64,

    /// Optional: compute unit limit (default 1_200_000)
    #[arg(long, default_value_t = 1_200_000)]
    pub cu_limit: u32,

    /// If provided, remove ALL liquidity for this position NFT mint (base58 Pubkey).
    #[arg(long)]
    pub remove_position: Option<String>,

    /// Min amount of token0 to receive when removing (default 0)
    #[arg(long, default_value_t = 0)]
    pub min_out0: u64,

    /// Min amount of token1 to receive when removing (default 0)
    #[arg(long, default_value_t = 0)]
    pub min_out1: u64,

    /// Also closes (burns) the position NFT after removing all liquidity
    #[arg(long)]
    pub close: bool,

    /// Raydium CLMM pool id (Pubkey base58) — required for open
    #[arg(long)]
    pub pool: Option<String>,

    /// Lower tick (must be multiple of pool.tick_spacing) — required for open
    #[arg(long)]
    pub lower: Option<i32>,

    /// Upper tick (must be multiple of pool.tick_spacing and > lower) — required for open
    #[arg(long)]
    pub upper: Option<i32>,

    /// Max amount of token0 to deposit (base units, u64; e.g., 1 SOL = 1_000_000_000)
    #[arg(long, default_value_t = 0)]
    pub amount0: u64,

    /// Max amount of token1 to deposit (base units, u64; e.g., 1 USDC = 1_000_000)
    #[arg(long, default_value_t = 0)]
    pub amount1: u64,

    /// Wrap this many lamports into WSOL (standalone if no open/remove args)
    #[arg(long, default_value_t = 0)]
    pub wrap_sol: u64,

    /// Unwrap WSOL ATA back to SOL (standalone if no open/remove args)
    #[arg(long, default_value_t = false)]
    pub unwrap_sol: bool,

    // --- SWAP mode ---
    /// Swap on this pool (Pubkey base58). When set, open/remove args are ignored.
    #[arg(long)]
    pub swap_pool: Option<String>,

    /// Swap input amount (base units)
    #[arg(long, default_value_t = 0)]
    pub swap_amount_in: u64,

    /// Minimum output amount (base units) to receive for the swap
    #[arg(long, default_value_t = 0)]
    pub swap_min_out: u64,

    /// Swap direction: true = token0 -> token1, false = token1 -> token0
    #[arg(long, default_value_t = true)]
    pub swap_a_to_b: bool,

    /// Optional sqrt price limit (Q64.64); default 0 uses protocol min/max
    #[arg(long, default_value_t = 0)]
    pub swap_sqrt_price_limit: u128,
}

/// Pick a DEX implementation.
#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum Dex {
    Raydium,
    Orca,
    Meteora,
}
