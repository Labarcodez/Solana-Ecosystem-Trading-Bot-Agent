//! Live price streaming via Shyft's (or any compatible provider's)
//! Yellowstone gRPC ("Dragon's Mouth") Geyser feed. Subscribes to account
//! updates for a set of AMM vault accounts and turns balance changes into
//! `PriceTick`s via `pool_price::price_from_vaults` - see that module for
//! why the vault-ratio approach works generically across DEXs without
//! decoding each program's own pool struct.
//!
//! NOTE: the exact field names on `SubscribeUpdateAccountInfo` are pinned
//! by whatever `yellowstone-grpc-proto` version is in `Cargo.lock` - this
//! module is written against the current stable proto shape and compiles
//! against it, but if a future proto bump renames a field, the compiler
//! will catch it here first.

use std::collections::HashMap;

use bot_core::{PriceTick, Pubkey};
use futures::{SinkExt, StreamExt};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use yellowstone_grpc_client::GeyserGrpcClient;
use yellowstone_grpc_proto::geyser::{subscribe_update::UpdateOneof, SubscribeRequest, SubscribeRequestFilterAccounts};

use crate::error::MarketDataError;
use crate::pool_price::{decode_token_account_amount, price_from_vaults};

/// One AMM-style pool to watch: two SPL Token vault accounts whose balance
/// ratio approximates the pool's spot price.
#[derive(Debug, Clone)]
pub struct WatchedPool {
    /// The token being priced.
    pub mint: Pubkey,
    pub base_vault: Pubkey,
    pub base_decimals: u8,
    /// Quote-currency vault (typically wSOL).
    pub quote_vault: Pubkey,
    pub quote_decimals: u8,
}

pub struct YellowstoneConfig {
    pub endpoint: String,
    pub x_token: String,
}

/// Connects, subscribes to account updates for every vault in `pools`, and
/// publishes a `PriceTick` on `tx` whenever a fresh reading is available for
/// both sides of a pool. Runs until `shutdown` is cancelled or the stream
/// ends.
pub async fn stream_prices(
    cfg: YellowstoneConfig,
    pools: Vec<WatchedPool>,
    tx: broadcast::Sender<PriceTick>,
    shutdown: CancellationToken,
) -> Result<(), MarketDataError> {
    let mut client = GeyserGrpcClient::build_from_shared(cfg.endpoint)
        .map_err(|e| MarketDataError::Grpc(e.to_string()))?
        .x_token(Some(cfg.x_token))
        .map_err(|e| MarketDataError::Grpc(e.to_string()))?
        .connect()
        .await
        .map_err(|e| MarketDataError::Grpc(e.to_string()))?;

    // vault pubkey (base58) -> (pool index, is this the base-side vault?)
    let mut vault_index: HashMap<String, (usize, bool)> = HashMap::new();
    let mut accounts_filter = Vec::new();
    for (i, pool) in pools.iter().enumerate() {
        vault_index.insert(pool.base_vault.to_string(), (i, true));
        vault_index.insert(pool.quote_vault.to_string(), (i, false));
        accounts_filter.push(pool.base_vault.to_string());
        accounts_filter.push(pool.quote_vault.to_string());
    }

    let mut filters = HashMap::new();
    filters.insert(
        "trading-bot-vaults".to_string(),
        SubscribeRequestFilterAccounts {
            account: accounts_filter,
            owner: vec![],
            filters: vec![],
            nonempty_txn_signature: None,
            cuckoo_accounts_filter: None,
        },
    );
    let request = SubscribeRequest { accounts: filters, ..Default::default() };

    let (mut sub_tx, mut stream) = client.subscribe().await.map_err(|e| MarketDataError::Grpc(e.to_string()))?;
    sub_tx.send(request).await.map_err(|e| MarketDataError::Grpc(e.to_string()))?;

    let mut base_balances: HashMap<usize, u64> = HashMap::new();
    let mut quote_balances: HashMap<usize, u64> = HashMap::new();

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            update = stream.next() => {
                let Some(update) = update else { break };
                let update = update.map_err(|e| MarketDataError::Grpc(e.to_string()))?;
                handle_update(update, &pools, &vault_index, &mut base_balances, &mut quote_balances, &tx);
            }
        }
    }

    Ok(())
}

fn handle_update(
    update: yellowstone_grpc_proto::geyser::SubscribeUpdate,
    pools: &[WatchedPool],
    vault_index: &HashMap<String, (usize, bool)>,
    base_balances: &mut HashMap<usize, u64>,
    quote_balances: &mut HashMap<usize, u64>,
    tx: &broadcast::Sender<PriceTick>,
) {
    let Some(UpdateOneof::Account(account_update)) = update.update_oneof else { return };
    let Some(info) = account_update.account else { return };
    if info.pubkey.len() != 32 {
        return;
    }
    let pubkey_str = bs58::encode(&info.pubkey).into_string();
    let Some(&(pool_idx, is_base)) = vault_index.get(&pubkey_str) else { return };
    let Ok(amount) = decode_token_account_amount(&info.data) else { return };

    if is_base {
        base_balances.insert(pool_idx, amount);
    } else {
        quote_balances.insert(pool_idx, amount);
    }

    if let (Some(&base_raw), Some(&quote_raw)) = (base_balances.get(&pool_idx), quote_balances.get(&pool_idx)) {
        let pool = &pools[pool_idx];
        if let Ok(price) = price_from_vaults(base_raw, pool.base_decimals, quote_raw, pool.quote_decimals) {
            let ts = chrono::Utc::now().timestamp();
            let _ = tx.send(PriceTick { mint: pool.mint, price, ts });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yellowstone_grpc_proto::geyser::{SubscribeUpdate, SubscribeUpdateAccount, SubscribeUpdateAccountInfo};

    fn fake_account_data(amount: u64) -> Vec<u8> {
        let mut data = vec![0u8; crate::pool_price::SPL_TOKEN_ACCOUNT_MIN_LEN];
        data[crate::pool_price::SPL_TOKEN_ACCOUNT_AMOUNT_OFFSET..crate::pool_price::SPL_TOKEN_ACCOUNT_AMOUNT_OFFSET + 8]
            .copy_from_slice(&amount.to_le_bytes());
        data
    }

    #[test]
    fn a_matching_pair_of_vault_updates_emits_one_price_tick() {
        let mint = Pubkey::new_unique();
        let base_vault = Pubkey::new_unique();
        let quote_vault = Pubkey::new_unique();
        let pools = vec![WatchedPool {
            mint,
            base_vault,
            base_decimals: 6,
            quote_vault,
            quote_decimals: 9,
        }];
        let mut vault_index = HashMap::new();
        vault_index.insert(base_vault.to_string(), (0usize, true));
        vault_index.insert(quote_vault.to_string(), (0usize, false));

        let mut base_balances = HashMap::new();
        let mut quote_balances = HashMap::new();
        let (tx, mut rx) = broadcast::channel(8);

        // Base-side update alone: not enough info yet for a price tick.
        let base_update = SubscribeUpdate {
            update_oneof: Some(UpdateOneof::Account(SubscribeUpdateAccount {
                account: Some(SubscribeUpdateAccountInfo {
                    pubkey: base_vault.to_bytes().to_vec(),
                    data: fake_account_data(100_000 * 10u64.pow(6)),
                    ..Default::default()
                }),
                slot: 1,
                is_startup: false,
            })),
            ..Default::default()
        };
        handle_update(base_update, &pools, &vault_index, &mut base_balances, &mut quote_balances, &tx);
        assert!(rx.try_recv().is_err(), "no tick expected before both sides are known");

        // Quote-side update completes the pair -> a tick should fire.
        let quote_update = SubscribeUpdate {
            update_oneof: Some(UpdateOneof::Account(SubscribeUpdateAccount {
                account: Some(SubscribeUpdateAccountInfo {
                    pubkey: quote_vault.to_bytes().to_vec(),
                    data: fake_account_data(1_000 * 10u64.pow(9)),
                    ..Default::default()
                }),
                slot: 2,
                is_startup: false,
            })),
            ..Default::default()
        };
        handle_update(quote_update, &pools, &vault_index, &mut base_balances, &mut quote_balances, &tx);

        let tick = rx.try_recv().expect("expected a price tick after both vault sides are known");
        assert_eq!(tick.mint, mint);
        assert!((tick.price - 0.01).abs() < 1e-9);
    }

    #[test]
    fn updates_for_unwatched_accounts_are_ignored() {
        let pools = vec![];
        let vault_index = HashMap::new();
        let mut base_balances = HashMap::new();
        let mut quote_balances = HashMap::new();
        let (tx, mut rx) = broadcast::channel(8);

        let update = SubscribeUpdate {
            update_oneof: Some(UpdateOneof::Account(SubscribeUpdateAccount {
                account: Some(SubscribeUpdateAccountInfo {
                    pubkey: Pubkey::new_unique().to_bytes().to_vec(),
                    data: fake_account_data(1),
                    ..Default::default()
                }),
                slot: 1,
                is_startup: false,
            })),
            ..Default::default()
        };
        handle_update(update, &pools, &vault_index, &mut base_balances, &mut quote_balances, &tx);
        assert!(rx.try_recv().is_err());
    }
}
