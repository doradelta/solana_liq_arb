# solana_liquidity_arb

Mainnet helper CLI for managing concentrated/decentralized liquidity and swaps on:

- Raydium CLMM
- Orca Whirlpools (CLMM)
- Meteora DLMM

It reuses a single set of CLI flags to:

- Open liquidity positions
- Remove/close existing positions
- Perform swaps
- Wrap/unwrap SOL to/from WSOL

All interactions are on Solana mainnet.

> ⚠️ This code is unaudited and operates with your private key on mainnet.  
> Use at your own risk and with small amounts first.

---

## Prerequisites

- Rust toolchain (stable) – install via [`rustup`](https://rustup.rs)
- Access to a Solana mainnet RPC endpoint
- A wallet whose private key you control (e.g. Phantom export)

The project targets the Solana 1.16.x SDK line and uses low‑level client crates for
Raydium, Orca Whirlpools, and Meteora.

---

## Building

Clone the repo and build the binary:

```bash
git clone <this-repo-url>
cd solana_liquidity_arb

cargo build --release
```

The resulting binary is at:

```bash
target/release/solana_liquidity_arb
```

You can also run directly with:

```bash
cargo run --release -- --help
```

---

## Configuration

The CLI uses environment variables and `.env` (via `dotenvy`) for configuration.

Create a `.env` file in the project root:

```env
PRIVATE_KEY_B58="base58-encoded-ed25519-keypair"
# Optional custom RPC endpoint (otherwise a public mainnet RPC is used)
RPC_URL="https://api.mainnet-beta.solana.com"
```

### `PRIVATE_KEY_B58`

- Required.
- Base58-encoded keypair used as the transaction payer & authority.
- Compatible with Phantom exports:
  - In Phantom: Settings → Developer → Export Private Key.
  - The exported base58 string can be used directly as `PRIVATE_KEY_B58`.
- Both 32‑byte seeds and 64‑byte keypairs are supported.

### `RPC_URL`

- Optional; if not set, a default public mainnet RPC URL is used.
- You can point this to a private or paid RPC for better reliability:

```env
RPC_URL="https://your-custom-rpc.example.com"
```

---

## CLI Overview

Global options (shared across DEXes) are defined in `src/cli.rs`:

- `--dex <raydium|orca|meteora>` – which DEX to target (default: `raydium`)
- `--rpc <URL>` – override `RPC_URL` from the environment
- `--cu-price <u64>` – microlamports per compute unit (default: `1000`)
- `--cu-limit <u32>` – compute unit limit (default: `1_200_000`)

Position management / liquidity:

- `--pool <PUBKEY>` – pool id:
  - Raydium: CLMM pool id
  - Orca: Whirlpool id
  - Meteora: `lb_pair` address
- `--lower <i32>` – lower tick / bin id (DEX‑specific)
- `--upper <i32>` – upper tick / bin id (DEX‑specific)
- `--amount0 <u64>` – max token0 amount to deposit (base units)
- `--amount1 <u64>` – max token1 amount to deposit (base units)
- `--remove-position <PUBKEY>` – position identifier:
  - Raydium & Orca: position NFT mint address
  - Meteora: Position account address
- `--min-out0 <u64>` – min token0 out when removing (Raydium only)
- `--min-out1 <u64>` – min token1 out when removing (Raydium only)
- `--close` – also close/burn the position (where supported)

Swap mode:

- `--swap-pool <PUBKEY>` – pool to swap on
- `--swap-amount-in <u64>` – input amount (base units)
- `--swap-min-out <u64>` – minimum amount out (slippage protection)
- `--swap-a-to-b <bool>` – swap direction:
  - `true` = token0 → token1 (or X → Y)
  - `false` = token1 → token0 (or Y → X)
- `--swap-sqrt-price-limit <u128>` – optional sqrt price limit:
  - `0` uses protocol defaults (min or max)

WSOL utilities:

- `--wrap-sol <u64>` – wrap this many lamports into WSOL
- `--unwrap-sol` – unwrap WSOL ATA back to native SOL

> Mode selection is automatic:
> - If `--swap-pool` is set → swap mode.  
> - Else if `--remove-position` is set → remove/close position.  
> - Else if `--pool` is set → open a new position.  
> - Otherwise, only wrap/unwrap instructions (if any) are sent.

---

## Usage Examples

Assume the binary is in `target/release/solana_liquidity_arb` and `.env`
contains `PRIVATE_KEY_B58` (and optionally `RPC_URL`).

Replace addresses and amounts with real values before running anything on mainnet.

### 1. Wrap & unwrap SOL

Wrap 0.1 SOL (100_000_000 lamports) into WSOL:

```bash
./target/release/solana_liquidity_arb \
  --wrap-sol 100000000
```

Unwrap your WSOL ATA back to SOL:

```bash
./target/release/solana_liquidity_arb \
  --unwrap-sol
```

You can combine wrap/unwrap with other modes; the helper will add the WSOL
instructions to the same transaction when possible.

### 2. Raydium CLMM – open a position

```bash
./target/release/solana_liquidity_arb \
  --dex raydium \
  --pool <RAYDIUM_POOL_ID> \
  --lower <LOWER_TICK> \
  --upper <UPPER_TICK> \
  --amount0 1000000000 \
  --amount1 1000000
```

Notes:

- `--lower` and `--upper` must align with the pool’s `tick_spacing` and `upper > lower`.
- `amount0`/`amount1` are in base units (e.g. `1 SOL = 1_000_000_000`).

### 3. Raydium CLMM – remove & optionally close position

```bash
./target/release/solana_liquidity_arb \
  --dex raydium \
  --remove-position <POSITION_NFT_MINT> \
  --min-out0 0 \
  --min-out1 0 \
  --close
```

- `--min-out0` / `--min-out1` are safety thresholds in base units.
- `--close` burns the position NFT once all liquidity is removed.

### 4. Orca Whirlpools – swap

```bash
./target/release/solana_liquidity_arb \
  --dex orca \
  --swap-pool <WHIRLPOOL_ID> \
  --swap-amount-in 1000000 \
  --swap-min-out 0 \
  --swap-a-to-b true
```

This will:

- Decode the Whirlpool account
- Ensure ATAs exist for both mints
- Build a `SwapV2` instruction using Orca’s on‑chain program

### 5. Meteora DLMM – open a position

```bash
./target/release/solana_liquidity_arb \
  --dex meteora \
  --pool <LB_PAIR_ADDRESS> \
  --lower <LOWER_BIN_ID> \
  --upper <UPPER_BIN_ID> \
  --amount0 1000000000 \
  --amount1 1000000
```

Internally this:

- Initializes a new position for `[lower, upper]` bin ids
- Distributes liquidity uniformly across the selected bins
- Adds liquidity via Meteora’s DLMM program

### 6. Meteora DLMM – remove & optionally close position

```bash
./target/release/solana_liquidity_arb \
  --dex meteora \
  --remove-position <POSITION_ACCOUNT> \
  --close
```

Here `--remove-position` is the position account (not an NFT mint).  
If `--close` is set, a `ClosePositionIfEmpty` instruction is added after
liquidity is removed.

---

## Development Notes

- Core entrypoint: `src/main.rs`
  - Dispatches to `raydium::run`, `orca::run`, or `meteora::run` based on `--dex`.
- CLI argument parsing: `src/cli.rs`
- DEX‑specific logic:
  - `src/raydium.rs` – Raydium CLMM helper
  - `src/orca.rs` – Orca Whirlpools helper
  - `src/meteora.rs` – Meteora DLMM helper
- Shared transaction helpers & WSOL utilities: `src/tx.rs`

To see all options and defaults:

```bash
cargo run -- --help
```

---

## Security & Disclaimer

- This repository is **not audited**.
- It signs and sends **mainnet** transactions using your private key.
- Always test with small amounts and/or on a forked mainnet environment first.
- Review the source code and understand the flows before automating or scaling usage.

