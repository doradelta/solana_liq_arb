use anyhow::Result;
use yellowstone_grpc_client::{GeyserGrpcClient, ClientTlsConfig};
use futures::{SinkExt, StreamExt};
use yellowstone_grpc_proto::prelude::*;
use yellowstone_grpc_proto::prelude::subscribe_update::UpdateOneof;
use log::{info, warn};

pub async fn run_watch(endpoint: &str, token: &str, pool: &str, position: &str) -> Result<()> {
    let mut client = GeyserGrpcClient::build_from_shared(endpoint.to_string())?
        .x_token(Some(token.to_string()))?
        .tls_config(ClientTlsConfig::new().with_native_roots())?
        .connect()
        .await?;

    let pool_pk = bs58::decode(pool).into_vec()?;
    let position_pk = bs58::decode(position).into_vec()?;

    // Subscribe to account updates (stable).
    let mut accounts = std::collections::HashMap::new();
    accounts.insert(
        "raydium_pool_and_position".to_string(),
        SubscribeRequestFilterAccounts {
            account: vec![
                String::from_utf8(bs58::encode(&pool_pk).into_string().into_bytes()).unwrap(),
                String::from_utf8(bs58::encode(&position_pk).into_string().into_bytes()).unwrap(),
            ],
            owner: vec![], // we filter by specific accounts
            filters: vec![],
            ..Default::default()
        },
    );

    let (mut subscribe_tx, mut subscribe_rx) = client.subscribe().await?;
    subscribe_tx.send(SubscribeRequest {
        accounts,
        commitment: Some(CommitmentLevel::Processed as i32),
        ..Default::default()
    }).await?;

    info!("Subscribed. Waiting for pool/position updates…");

    while let Some(msg) = subscribe_rx.next().await {
        match msg {
            Ok(update) => {
                if let Some(UpdateOneof::Account(account_update)) = update.update_oneof {
                    // account_update.account contains updated data;
                    // decode PoolState / PersonalPositionState, compute your amounts at current sqrt_price
                    // If computed token0/token1 split differs (and the ‘non-deposit’ side becomes > 0), print/notify.
                    info!("Account updated at slot {}", account_update.slot);
                }
            }
            Err(e) => warn!("stream error: {:?}", e),
        }
    }
    Ok(())
}
