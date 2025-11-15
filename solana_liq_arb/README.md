# raydium-liquidity-rs

Small experimental CLI to interact with Raydium CLMM positions: open/add/remove liquidity and watch fills over Yellowstone gRPC.

## Status

- `cache-pool` and `open` are wired to the decoder; `open` mints a position NFT and sends `open_position_v2`.
- `add` / `remove` are still stubbed.
- PDAs/program IDs are derived from the pool owner; helpers in `src/pool.rs` remain TODOs for richer pool info. Personal position PDA seeds use `[b"personal_position", pool, position_mint]`.

## Prerequisites

- Rust toolchain (1.79+ recommended).
- Solana CLI keypair for fee payer (or Phantom export, see below).
- Yellowstone gRPC endpoint + X-Token (e.g., QuickNode).

## Build

```
cargo build --release
```

Note: builds target `/tmp/solana-liq-arb-target` via `.cargo/config.toml` to avoid filename-length issues on encrypted home directories.

## Usage

All commands share the top-level flags:

- `--rpc-url` (or env `RPC_URL`)
- `--payer` (or env `PAYER`, default `~/.config/solana/id.json`)

### Cache a pool locally

Decode and cache pool state to `pool-cache/<POOL>.json`:

```
./target/release/solana_liq_arb \
  cache-pool \
  --rpc-url https://api.mainnet-beta.solana.com \
  --pool <POOL_PUBKEY> \
  --refresh
```

### Payer key formats

`PAYER` can be:
- A file path to a Solana JSON keypair (default behavior; `~` is expanded).
- A base58-encoded 64-byte secret key (Phantom export format).
- A JSON array of 64 bytes (Phantom export).

### Open a position (stubbed)

```
PAYER="BASE58_64_BYTE_SECRET_FROM_PHANTOM" \
  ./target/release/solana_liq_arb \
    --rpc-url https://api.mainnet-beta.solana.com \
    open \
    --pool <POOL_PUBKEY> \
    --price-min 1.0 \
    --price-max 1.1 \
    --amount0-max 1000000 \
    --amount1-max 0
```

You would capture:
- Personal position PDA
- Position NFT mint
- Transaction signature

### Add liquidity (stubbed)

```
./target/release/solana_liq_arb \
  --rpc-url ... --payer ... \
  add \
  --pool <POOL_PUBKEY> \
  --position <PERSONAL_POSITION_PDA> \
  --nft-mint <POSITION_NFT_MINT> \
  --amount0-max 0 \
  --amount1-max 50000000
```

### Remove liquidity (stubbed)

```
./target/release/solana_liq_arb \
  --rpc-url ... --payer ... \
  remove \
  --pool <POOL_PUBKEY> \
  --position <PERSONAL_POSITION_PDA> \
  --nft-mint <POSITION_NFT_MINT> \
  --liquidity <U128_LIQUIDITY>
```

### Watch fills (implemented)

Streams pool + personal position accounts over Yellowstone and logs updates:

```
./target/release/solana_liq_arb \
  watch-fill \
  --endpoint https://YOUR-YELLOWSTONE-ENDPOINT:10000 \
  --token YOUR_X_TOKEN \
  --pool <POOL_PUBKEY> \
  --position <PERSONAL_POSITION_PDA>
```

## Next steps / TODOs

- Fill in Raydium program IDs and PDA derivations (`src/pda.rs`).
- Implement pool fetch/decoding and tick-array/vault helpers (`src/pool.rs`, `src/open_cmd.rs`).
- Wire `open`, `add`, and `remove` flows to the current decoder API.
- Add minimal integration tests once flows are implemented.
