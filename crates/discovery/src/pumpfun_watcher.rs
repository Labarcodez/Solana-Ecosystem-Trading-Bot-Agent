//! Real-time discovery via Yellowstone gRPC: subscribes to
//! (a) transactions touching the pump.fun program, to catch `create`
//! instructions (new token launches), and (b) account updates owned by the
//! pump.fun program, to catch a bonding curve's `complete` flag flipping to
//! `true` (graduation) - without needing to know every bonding-curve
//! address up front, since Yellowstone's account filter can match by
//! `owner` alone.

use std::collections::HashMap;

use bot_core::Pubkey;
use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use yellowstone_grpc_client::GeyserGrpcClient;
use yellowstone_grpc_proto::geyser::{
    subscribe_update::UpdateOneof, SubscribeRequest, SubscribeRequestFilterAccounts,
    SubscribeRequestFilterTransactions,
};

use crate::bonding_curve::{decode_bonding_curve, find_mint_and_bonding_curve, CREATE_DISCRIMINATOR};
use crate::error::DiscoveryError;

pub struct GrpcConfig {
    pub endpoint: String,
    pub x_token: String,
}

/// A raw sighting from the gRPC stream, before it's been resolved against
/// the `MintRegistry`. Kept separate from `registry::DiscoveryEvent` because
/// a curve-completion sighting only carries the curve's own pubkey - only
/// the registry (which remembers mint<->curve pairs from the `create`
/// sighting) can resolve that back to a mint.
#[derive(Debug, Clone, PartialEq)]
pub enum WatcherEvent {
    Created { mint: Pubkey, curve_pda: Pubkey, ts: i64 },
    CurveCompleted { curve_pda: Pubkey, ts: i64 },
}

/// Connects and streams `WatcherEvent`s on `events_tx` until `shutdown` is
/// cancelled or the stream ends. Resolving these into `DiscoveryEvent`s
/// (and deciding what to do with them) is the caller's job - see
/// `Discovery::apply_watcher_event` in `lib.rs`.
pub async fn watch(
    cfg: GrpcConfig,
    program_id: Pubkey,
    events_tx: mpsc::UnboundedSender<WatcherEvent>,
    shutdown: CancellationToken,
) -> Result<(), DiscoveryError> {
    let mut client = GeyserGrpcClient::build_from_shared(cfg.endpoint)
        .map_err(|e| DiscoveryError::Grpc(e.to_string()))?
        .x_token(Some(cfg.x_token))
        .map_err(|e| DiscoveryError::Grpc(e.to_string()))?
        .connect()
        .await
        .map_err(|e| DiscoveryError::Grpc(e.to_string()))?;

    let program_id_str = program_id.to_string();

    let mut tx_filters = HashMap::new();
    tx_filters.insert(
        "pumpfun-create".to_string(),
        SubscribeRequestFilterTransactions {
            vote: Some(false),
            failed: Some(false),
            account_include: vec![program_id_str.clone()],
            ..Default::default()
        },
    );

    let mut account_filters = HashMap::new();
    account_filters.insert(
        "pumpfun-curves".to_string(),
        SubscribeRequestFilterAccounts {
            account: vec![],
            owner: vec![program_id_str],
            filters: vec![],
            nonempty_txn_signature: None,
            cuckoo_accounts_filter: None,
        },
    );

    let request = SubscribeRequest { transactions: tx_filters, accounts: account_filters, ..Default::default() };

    let (mut sub_tx, mut stream) = client.subscribe().await.map_err(|e| DiscoveryError::Grpc(e.to_string()))?;
    sub_tx.send(request).await.map_err(|e| DiscoveryError::Grpc(e.to_string()))?;

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            update = stream.next() => {
                let Some(update) = update else { break };
                let update = update.map_err(|e| DiscoveryError::Grpc(e.to_string()))?;
                let now_ts = chrono::Utc::now().timestamp();
                if let Some(event) = handle_update(update, &program_id, now_ts) {
                    let _ = events_tx.send(event);
                }
            }
        }
    }

    Ok(())
}

fn pubkey_from_bytes(bytes: &[u8]) -> Option<Pubkey> {
    let arr: [u8; 32] = bytes.try_into().ok()?;
    Some(Pubkey::from(arr))
}

fn handle_update(
    update: yellowstone_grpc_proto::geyser::SubscribeUpdate,
    program_id: &Pubkey,
    now_ts: i64,
) -> Option<WatcherEvent> {
    match update.update_oneof? {
        UpdateOneof::Transaction(tx_update) => handle_transaction(tx_update, program_id, now_ts),
        UpdateOneof::Account(account_update) => handle_account(account_update, now_ts),
        _ => None,
    }
}

fn handle_transaction(
    tx_update: yellowstone_grpc_proto::geyser::SubscribeUpdateTransaction,
    program_id: &Pubkey,
    now_ts: i64,
) -> Option<WatcherEvent> {
    let info = tx_update.transaction?;
    let tx = info.transaction?;
    let message = tx.message?;
    let account_keys: Vec<Pubkey> = message.account_keys.iter().filter_map(|k| pubkey_from_bytes(k)).collect();

    for ix in &message.instructions {
        if ix.data.len() < 8 || ix.data[0..8] != CREATE_DISCRIMINATOR {
            continue;
        }
        // A `create`-shaped instruction on the pump.fun program - find the
        // (mint, bonding_curve) pair by PDA derivation rather than a
        // hardcoded account index (see bonding_curve.rs for why).
        if let Some((mint, curve_pda)) = find_mint_and_bonding_curve(program_id, &account_keys) {
            return Some(WatcherEvent::Created { mint, curve_pda, ts: now_ts });
        }
    }
    None
}

fn handle_account(
    account_update: yellowstone_grpc_proto::geyser::SubscribeUpdateAccount,
    now_ts: i64,
) -> Option<WatcherEvent> {
    let info = account_update.account?;
    let state = decode_bonding_curve(&info.data).ok()?;
    if !state.complete {
        return None;
    }
    let curve_pda = pubkey_from_bytes(&info.pubkey)?;
    Some(WatcherEvent::CurveCompleted { curve_pda, ts: now_ts })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bonding_curve::bonding_curve_pda;
    use yellowstone_grpc_proto::geyser::{
        SubscribeUpdate, SubscribeUpdateAccount, SubscribeUpdateAccountInfo, SubscribeUpdateTransaction,
        SubscribeUpdateTransactionInfo,
    };
    use yellowstone_grpc_proto::solana::storage::confirmed_block::{CompiledInstruction, Message, Transaction};

    #[test]
    fn detects_create_instruction_and_extracts_mint() {
        let program_id = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let (curve_pda, _) = bonding_curve_pda(&program_id, &mint);
        let unrelated = Pubkey::new_unique();

        let account_keys = vec![
            unrelated.to_bytes().to_vec(),
            mint.to_bytes().to_vec(),
            curve_pda.to_bytes().to_vec(),
        ];
        let mut data = CREATE_DISCRIMINATOR.to_vec();
        data.extend_from_slice(b"padding-args");

        let message = Message {
            account_keys,
            instructions: vec![CompiledInstruction { program_id_index: 0, accounts: vec![], data }],
            ..Default::default()
        };
        let tx = Transaction { message: Some(message), ..Default::default() };
        let update = SubscribeUpdate {
            update_oneof: Some(UpdateOneof::Transaction(SubscribeUpdateTransaction {
                transaction: Some(SubscribeUpdateTransactionInfo { transaction: Some(tx), ..Default::default() }),
                slot: 1,
            })),
            ..Default::default()
        };

        let event = handle_update(update, &program_id, 1000);
        assert_eq!(event, Some(WatcherEvent::Created { mint, curve_pda, ts: 1000 }));
    }

    #[test]
    fn ignores_transactions_without_the_create_discriminator() {
        let program_id = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let account_keys = vec![mint.to_bytes().to_vec()];
        let message = Message {
            account_keys,
            instructions: vec![CompiledInstruction { program_id_index: 0, accounts: vec![], data: vec![1, 2, 3] }],
            ..Default::default()
        };
        let tx = Transaction { message: Some(message), ..Default::default() };
        let update = SubscribeUpdate {
            update_oneof: Some(UpdateOneof::Transaction(SubscribeUpdateTransaction {
                transaction: Some(SubscribeUpdateTransactionInfo { transaction: Some(tx), ..Default::default() }),
                slot: 1,
            })),
            ..Default::default()
        };
        assert!(handle_update(update, &program_id, 1000).is_none());
    }

    #[test]
    fn detects_graduation_from_a_completed_bonding_curve_account() {
        let mut data = vec![0u8; crate::bonding_curve::BONDING_CURVE_ACCOUNT_MIN_LEN];
        data[0x30] = 1; // complete = true
        let curve_pubkey = Pubkey::new_unique();

        let update = SubscribeUpdate {
            update_oneof: Some(UpdateOneof::Account(SubscribeUpdateAccount {
                account: Some(SubscribeUpdateAccountInfo {
                    pubkey: curve_pubkey.to_bytes().to_vec(),
                    data,
                    ..Default::default()
                }),
                slot: 1,
                is_startup: false,
            })),
            ..Default::default()
        };
        let program_id = Pubkey::new_unique();
        let event = handle_update(update, &program_id, 2000);
        assert_eq!(event, Some(WatcherEvent::CurveCompleted { curve_pda: curve_pubkey, ts: 2000 }));
    }

    #[test]
    fn incomplete_bonding_curve_account_produces_no_event() {
        let data = vec![0u8; crate::bonding_curve::BONDING_CURVE_ACCOUNT_MIN_LEN]; // complete = false
        let update = SubscribeUpdate {
            update_oneof: Some(UpdateOneof::Account(SubscribeUpdateAccount {
                account: Some(SubscribeUpdateAccountInfo {
                    pubkey: Pubkey::new_unique().to_bytes().to_vec(),
                    data,
                    ..Default::default()
                }),
                slot: 1,
                is_startup: false,
            })),
            ..Default::default()
        };
        let program_id = Pubkey::new_unique();
        assert!(handle_update(update, &program_id, 2000).is_none());
    }
}
