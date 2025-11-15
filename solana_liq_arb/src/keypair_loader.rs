use std::path::Path;

use anyhow::{anyhow, Context, Result};
use bs58;
use solana_sdk::{
    signature::{read_keypair_file, Keypair},
};
use std::convert::TryFrom;

/// Load a keypair from either a file path, a base58-encoded 64-byte secret key (Phantom export),
/// or a JSON array of 64 bytes.
pub fn load_keypair(input: &str) -> Result<Keypair> {
    let expanded = shellexpand::tilde(input).to_string();
    let path = Path::new(&expanded);
    if path.exists() {
        return read_keypair_file(path)
            .map_err(|e| anyhow!("read keypair file at {}: {e}", expanded));
    }

    // Try JSON array (e.g., [12,34,...]).
    if input.trim_start().starts_with('[') {
        let vec: Vec<u8> = serde_json::from_str(input).context("parse keypair JSON array")?;
        let bytes: [u8; 64] =
            vec.try_into().map_err(|_| anyhow!("expected 64-byte keypair array"))?;
        return Keypair::try_from(&bytes[..]).map_err(|e| anyhow!("build keypair from JSON array: {e}"));
    }

    // Fallback: base58-encoded secret (common Phantom export format).
    let decoded = bs58::decode(input.trim()).into_vec().map_err(|e| anyhow!("base58 decode payer: {e}"))?;
    let bytes: [u8; 64] =
        decoded.try_into().map_err(|_| anyhow!("base58: expected 64-byte secret key"))?;
    Keypair::try_from(&bytes[..]).map_err(|e| anyhow!("keypair from base58: {e}"))
}
