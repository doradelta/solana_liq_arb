use anyhow::Result;
use clap::Parser;
use dotenvy::dotenv;

mod cli;
mod raydium;
mod orca;
mod meteora;
mod tx;

fn main() -> Result<()> {
    dotenv().ok();
    let opts = cli::Opts::parse();
    match opts.dex {
        cli::Dex::Raydium => raydium::run(opts),
        cli::Dex::Orca => orca::run(opts),
        cli::Dex::Meteora => meteora::run(opts),
    }
}
