use solana_sdk::pubkey::Pubkey;

/// Raydium CLMM program id (mainnet)
pub const RAYDIUM_CLMM_PROGRAM: Pubkey = Pubkey::new_from_array([
    // you should set to current Raydium Amm v3 program id for mainnet
    // (fetchable from Raydium docs / repo)
    // placeholder: replace with the real one you're targeting
    0; 32
]);

/// Metaplex Token Metadata program id (mainnet)
pub const METADATA_PROGRAM_ID: Pubkey = Pubkey::from_str_const(
    "metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s",
);

pub fn metadata_pda(mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            b"metadata",
            METADATA_PROGRAM_ID.as_ref(),
            mint.as_ref()
        ],
        &METADATA_PROGRAM_ID
    )
}

/// NOTE: Seeds here reflect Raydium’s SDK helpers; keep in sync if SDK updates.
/// If you prefer, call the SDK in a one-off script to verify addresses before submitting.
pub fn personal_position_pda(nft_mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            b"personal_position",
            nft_mint.as_ref(),
        ],
        &RAYDIUM_CLMM_PROGRAM
    )
}

pub fn protocol_position_pda(pool: &Pubkey, tick_lower: i32, tick_upper: i32) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            b"protocol_position",
            pool.as_ref(),
            &tick_lower.to_le_bytes(),
            &tick_upper.to_le_bytes(),
        ],
        &RAYDIUM_CLMM_PROGRAM
    )
}
