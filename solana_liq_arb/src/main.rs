use clap::{Parser, Subcommand};
use anyhow::Result;
use std::str::FromStr;

mod pda;
mod pool;
mod open_cmd;
mod add_remove_cmd;
mod watch_fill;
mod keypair_loader;
mod pool_cache;

#[derive(Parser)]
#[command(name = "raydium-liquidity-rs")]
#[command(about = "Open/add/remove CLMM liquidity and watch fills (Raydium, Solana)")]
struct Cli {
    /// JSON-RPC URL (e.g., https://api.mainnet-beta.solana.com)
    #[arg(long, env="RPC_URL")]
    rpc_url: String,

    /// Fee payer keypair path (e.g., ~/.config/solana/id.json)
    #[arg(long, env="PAYER", default_value = "~/.config/solana/id.json")]
    payer: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Cache pool state locally (decoded) for reuse
    CachePool {
        #[arg(long)] pool: String,
        /// overwrite existing cache
        #[arg(long, default_value_t=false)] refresh: bool,
    },

    /// Open a brand new CLMM position (mints the position NFT)
    Open {
        #[arg(long)] pool: String,
        /// price range in terms of token1 per token0, OR
        #[arg(long)] price_min: Option<f64>,
        #[arg(long)] price_max: Option<f64>,
        /// or supply ticks directly:
        #[arg(long)] tick_lower: Option<i32>,
        #[arg(long)] tick_upper: Option<i32>,

        /// Max amounts in base units (u64). Set one to 0 for one-sided deposit.
        #[arg(long)] amount0_max: u64,
        #[arg(long)] amount1_max: u64,
    },

    /// Add liquidity to an existing position
    Add {
        #[arg(long)] pool: String,
        #[arg(long)] position: String,   // personal_position PDA
        #[arg(long)] nft_mint: String,   // position NFT mint
        #[arg(long)] amount0_max: u64,
        #[arg(long)] amount1_max: u64,
    },

    /// Remove liquidity from an existing position
    Remove {
        #[arg(long)] pool: String,
        #[arg(long)] position: String,
        #[arg(long)] nft_mint: String,
        #[arg(long)] liquidity: u128,
    },

    /// Watch in real time when one-sided liquidity starts/continues converting
    WatchFill {
        /// Yellowstone endpoint like: https://...quiknode.pro:10000
        #[arg(long, env="YELLOWSTONE_ENDPOINT")] endpoint: String,
        /// Your X-Token from the Yellowstone URL
        #[arg(long, env="YELLOWSTONE_TOKEN")] token: String,

        /// CLMM pool id
        #[arg(long)] pool: String,
        /// Your personal position PDA (created when you opened)
        #[arg(long)] position: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();

    match cli.command {
        Commands::CachePool { pool, refresh } => {
            use solana_client::rpc_client::RpcClient;
            use solana_sdk::pubkey::Pubkey;
            let rpc = RpcClient::new(cli.rpc_url.clone());
            let pool_id = Pubkey::from_str(&pool)?;
            let snap = if refresh {
                pool_cache::refresh_sync(&rpc, &pool_id)?
            } else {
                pool_cache::get_or_fetch_sync(&rpc, &pool_id, false)?
            };
            let path = pool_cache::cache_file_path(&pool_id);
            println!("Cached pool {} -> {}", pool_id, path.display());
            println!(
                "token0={}, token1={}, vault0={}, vault1={}, decimals=({},{}) tick_spacing={}",
                snap.token_mint0, snap.token_mint1, snap.token_vault0, snap.token_vault1,
                snap.mint_decimals0, snap.mint_decimals1, snap.tick_spacing
            );
        }
        Commands::Open { pool, price_min, price_max, tick_lower, tick_upper, amount0_max, amount1_max } => {
            open_cmd::run_open(&cli.rpc_url, &cli.payer, &pool, price_min, price_max, tick_lower, tick_upper, amount0_max, amount1_max).await?
        }
        Commands::Add { pool, position, nft_mint, amount0_max, amount1_max } => {
            add_remove_cmd::run_add(&cli.rpc_url, &cli.payer, &pool, &position, &nft_mint, amount0_max, amount1_max).await?
        }
        Commands::Remove { pool, position, nft_mint, liquidity } => {
            add_remove_cmd::run_remove(&cli.rpc_url, &cli.payer, &pool, &position, &nft_mint, liquidity).await?
        }
        Commands::WatchFill { endpoint, token, pool, position } => {
            watch_fill::run_watch(&endpoint, &token, &pool, &position).await?
        }
    }
    Ok(())
}
