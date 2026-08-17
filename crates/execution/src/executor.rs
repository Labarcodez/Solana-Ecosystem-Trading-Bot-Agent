//! Ties the Jupiter, Jito, and pump.fun clients together behind one
//! `Executor`, routed by `ApprovedOrder.token_meta.phase`. `dry_run` is
//! first-class, not an afterthought: both paths make their real quote call
//! (proving the integration is live) and stop before anything is signed or
//! submitted.

use std::str::FromStr;

use bot_core::{ApprovedOrder, Fill, Side, TokenPhase};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::instruction::Instruction;
use solana_sdk::signature::{Keypair, Signer};
use solana_sdk::transaction::{Transaction, VersionedTransaction};
use solana_system_interface::instruction as system_instruction;

use crate::error::ExecutionError;
use crate::jito::JitoClient;
use crate::jupiter::{JupiterClient, QuoteResponse};
use crate::pumpfun::{self, PumpFunAccounts};

const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";
const LAMPORTS_PER_SOL: f64 = 1_000_000_000.0;
const JITO_TIP_LAMPORTS: u64 = 10_000;

/// Everything about the bonding curve's current state the executor needs
/// for a pump.fun quote/trade - supplied by the caller (from
/// `market_data`/`discovery`'s last-known reading or a fresh RPC fetch)
/// rather than fetched internally, so `Executor` doesn't need its own RPC
/// client just for pricing.
#[derive(Debug, Clone, Copy)]
pub struct BondingCurveContext {
    pub virtual_token_reserves: u64,
    pub virtual_sol_reserves: u64,
    /// From the bonding-curve account's own `creator` field (see
    /// `discovery::bonding_curve::BondingCurveState::creator`) - required
    /// to derive `creator_vault`.
    pub creator: bot_core::Pubkey,
    /// The `global` config account's current `fee_recipient`. Best sourced
    /// from a recent real transaction or the `global` account itself; the
    /// caller is expected to keep this current since it's not something
    /// `Executor` looks up on its own.
    pub fee_recipient: bot_core::Pubkey,
    /// Whichever SPL token program actually owns this mint - pump.fun
    /// mints can be classic SPL Token or Token-2022, and the associated
    /// token account addresses differ depending on which (see
    /// `pumpfun::TOKEN_PROGRAM_ID` / `TOKEN_2022_PROGRAM_ID`).
    pub token_program: bot_core::Pubkey,
}

pub struct Executor {
    jupiter: JupiterClient,
    jito: JitoClient,
    dry_run: bool,
    wsol_mint: bot_core::Pubkey,
    /// Needed only for the live pump.fun path, to fetch a recent blockhash
    /// for a self-built transaction (the Jupiter path doesn't need this -
    /// Jupiter's `/swap` response already embeds a valid blockhash).
    /// Defaults to `ALCHEMY_RPC_URL` from the environment.
    rpc_url: Option<String>,
}

impl Executor {
    pub fn new(dry_run: bool) -> Self {
        Self::with_rpc_url(dry_run, std::env::var("ALCHEMY_RPC_URL").ok())
    }

    pub fn with_rpc_url(dry_run: bool, rpc_url: Option<String>) -> Self {
        Self {
            jupiter: JupiterClient::new(),
            jito: JitoClient::new(),
            dry_run,
            wsol_mint: bot_core::Pubkey::from_str(WSOL_MINT).expect("valid static mint"),
            rpc_url,
        }
    }

    pub fn is_dry_run(&self) -> bool {
        self.dry_run
    }

    /// Executes an order whose token has graduated (`TokenPhase::Migrated`)
    /// via Jupiter (+ Jito for live landing). `mark_price` (SOL per token)
    /// and `mint_decimals` are needed to convert a sell's SOL-denominated
    /// `size_sol` into the raw token amount Jupiter's `/quote` expects.
    pub async fn execute(
        &self,
        order: &ApprovedOrder,
        mark_price: f64,
        mint_decimals: u8,
        wallet: Option<&Keypair>,
        bonding_curve: Option<BondingCurveContext>,
    ) -> Result<Fill, ExecutionError> {
        match order.token_meta.phase {
            TokenPhase::Migrated => self.execute_migrated(order, mark_price, mint_decimals, wallet).await,
            TokenPhase::BondingCurve => {
                let ctx = bonding_curve.ok_or_else(|| {
                    ExecutionError::UnexpectedResponse("bonding-curve order requires BondingCurveContext".into())
                })?;
                self.execute_bonding_curve(order, mark_price, mint_decimals, ctx, wallet).await
            }
        }
    }

    async fn execute_migrated(
        &self,
        order: &ApprovedOrder,
        mark_price: f64,
        mint_decimals: u8,
        wallet: Option<&Keypair>,
    ) -> Result<Fill, ExecutionError> {
        let (input_mint, output_mint, amount) = match order.side {
            Side::Buy => (self.wsol_mint, order.mint, (order.size_sol * LAMPORTS_PER_SOL).round() as u64),
            Side::Sell => {
                let ui_qty = if mark_price > 0.0 { order.size_sol / mark_price } else { 0.0 };
                let raw_qty = (ui_qty * 10f64.powi(mint_decimals as i32)).round() as u64;
                (order.mint, self.wsol_mint, raw_qty)
            }
        };

        // Real, live quote call regardless of mode - this is what proves
        // the integration works even in dry_run.
        let quote = self.jupiter.quote(&input_mint, &output_mint, amount, order.max_slippage_bps).await?;

        if self.dry_run {
            tracing::info!(
                mint = %order.mint, side = ?order.side, in_amount = quote.in_amount, out_amount = quote.out_amount,
                "[DRY-RUN] would swap via Jupiter"
            );
            return Ok(fill_from_jupiter_quote(order, &quote, mint_decimals, true, None));
        }

        let Some(wallet) = wallet else {
            return Err(ExecutionError::UnexpectedResponse("live execution requires an unlocked wallet".into()));
        };
        let swap_tx_b64 = self.jupiter.swap_transaction_base64(&quote, &wallet.pubkey()).await?;
        let bundle_id = self.sign_and_submit_jupiter_via_jito(swap_tx_b64, wallet).await?;
        tracing::info!(mint = %order.mint, bundle_id, "submitted live swap via Jito bundle");
        Ok(fill_from_jupiter_quote(order, &quote, mint_decimals, false, Some(bundle_id)))
    }

    /// Live path is IDL-verified as of this pass (see `pumpfun.rs` module
    /// docs for exactly what was cross-checked against real mainnet
    /// transactions and what residual uncertainty remains). Still gated
    /// behind having a real wallet and an RPC URL to fetch a blockhash from -
    /// neither of which this sandbox has, so this path is real, complete,
    /// unit-tested code that has not itself been exercised against mainnet
    /// with real funds.
    async fn execute_bonding_curve(
        &self,
        order: &ApprovedOrder,
        mark_price: f64,
        mint_decimals: u8,
        ctx: BondingCurveContext,
        wallet: Option<&Keypair>,
    ) -> Result<Fill, ExecutionError> {
        let (qty_in, is_buy) = match order.side {
            Side::Buy => ((order.size_sol * LAMPORTS_PER_SOL).round() as u64, true),
            Side::Sell => {
                let ui_qty = if mark_price > 0.0 { order.size_sol / mark_price } else { 0.0 };
                ((ui_qty * 10f64.powi(mint_decimals as i32)).round() as u64, false)
            }
        };

        let out_raw = if is_buy {
            pumpfun::quote_buy(ctx.virtual_token_reserves, ctx.virtual_sol_reserves, qty_in)?
        } else {
            pumpfun::quote_sell(ctx.virtual_token_reserves, ctx.virtual_sol_reserves, qty_in)?
        };

        if self.dry_run {
            tracing::info!(
                mint = %order.mint, side = ?order.side, in_raw = qty_in, out_raw,
                "[DRY-RUN] would trade via pump.fun bonding curve"
            );
            return Ok(fill_from_bonding_curve_quote(order, qty_in, out_raw, mint_decimals, is_buy, true, None));
        }

        let Some(wallet) = wallet else {
            return Err(ExecutionError::UnexpectedResponse("live execution requires an unlocked wallet".into()));
        };
        let Some(rpc_url) = &self.rpc_url else {
            return Err(ExecutionError::UnexpectedResponse(
                "live bonding-curve trading requires an RPC URL (ALCHEMY_RPC_URL) to fetch a recent blockhash".into(),
            ));
        };

        let accounts =
            PumpFunAccounts::derive(order.mint, wallet.pubkey(), ctx.creator, ctx.fee_recipient, ctx.token_program);
        let slippage = order.max_slippage_bps as f64 / 10_000.0;
        let ix = if is_buy {
            let max_sol_cost = (qty_in as f64 * (1.0 + slippage)).round() as u64;
            let min_token_out = (out_raw as f64 * (1.0 - slippage)).round() as u64;
            pumpfun::buy_instruction(&accounts, min_token_out, max_sol_cost)
        } else {
            let min_sol_output = (out_raw as f64 * (1.0 - slippage)).round() as u64;
            pumpfun::sell_instruction(&accounts, qty_in, min_sol_output)
        };

        let rpc = RpcClient::new(rpc_url.clone());
        let recent_blockhash =
            rpc.get_latest_blockhash().await.map_err(|e| ExecutionError::UnexpectedResponse(e.to_string()))?;

        let bundle_id = self.sign_and_submit_legacy_via_jito(ix, wallet, recent_blockhash).await?;
        tracing::info!(mint = %order.mint, bundle_id, "submitted live pump.fun trade via Jito bundle");
        Ok(fill_from_bonding_curve_quote(order, qty_in, out_raw, mint_decimals, is_buy, false, Some(bundle_id)))
    }

    /// Builds and signs the Jito tip transfer every live bundle needs -
    /// shared by both the Jupiter and pump.fun live paths.
    async fn tip_transaction(&self, wallet: &Keypair, recent_blockhash: solana_sdk::hash::Hash) -> Result<Transaction, ExecutionError> {
        let tip_accounts = self.jito.get_tip_accounts().await?;
        let tip_account =
            tip_accounts.first().copied().ok_or_else(|| ExecutionError::Jito("no tip accounts returned".into()))?;
        let tip_ix = system_instruction::transfer(&wallet.pubkey(), &tip_account, JITO_TIP_LAMPORTS);
        Ok(Transaction::new_signed_with_payer(&[tip_ix], Some(&wallet.pubkey()), &[wallet], recent_blockhash))
    }

    /// Signs `swap_tx_b64` (from Jupiter's `/swap`) with `wallet`, wraps it
    /// with a Jito tip transfer, and submits the bundle.
    async fn sign_and_submit_jupiter_via_jito(&self, swap_tx_b64: String, wallet: &Keypair) -> Result<String, ExecutionError> {
        let tx_bytes = base64_decode(&swap_tx_b64)?;
        let versioned_tx: VersionedTransaction =
            bincode::deserialize(&tx_bytes).map_err(|e| ExecutionError::UnexpectedResponse(e.to_string()))?;
        let signed = sign_versioned_transaction(versioned_tx, wallet)?;
        let recent_blockhash = *signed.message.recent_blockhash();
        let tip_tx = self.tip_transaction(wallet, recent_blockhash).await?;
        let signed_txs = vec![
            bs58::encode(bincode::serialize(&tip_tx).expect("transaction serializes")).into_string(),
            bs58::encode(bincode::serialize(&signed).expect("transaction serializes")).into_string(),
        ];
        self.jito.send_bundle(signed_txs).await
    }

    /// Builds, signs, and submits a self-constructed legacy `Transaction`
    /// (the pump.fun bonding-curve path) wrapped with a Jito tip transfer.
    async fn sign_and_submit_legacy_via_jito(
        &self,
        ix: Instruction,
        wallet: &Keypair,
        recent_blockhash: solana_sdk::hash::Hash,
    ) -> Result<String, ExecutionError> {
        let main_tx = Transaction::new_signed_with_payer(&[ix], Some(&wallet.pubkey()), &[wallet], recent_blockhash);
        let tip_tx = self.tip_transaction(wallet, recent_blockhash).await?;
        let signed_txs = vec![
            bs58::encode(bincode::serialize(&tip_tx).expect("transaction serializes")).into_string(),
            bs58::encode(bincode::serialize(&main_tx).expect("transaction serializes")).into_string(),
        ];
        self.jito.send_bundle(signed_txs).await
    }
}

fn base64_decode(s: &str) -> Result<Vec<u8>, ExecutionError> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    STANDARD.decode(s).map_err(|e| ExecutionError::Base64(e.to_string()))
}

fn sign_versioned_transaction(
    tx: VersionedTransaction,
    wallet: &Keypair,
) -> Result<VersionedTransaction, ExecutionError> {
    VersionedTransaction::try_new(tx.message, &[wallet])
        .map_err(|e| ExecutionError::UnexpectedResponse(format!("failed to sign transaction: {e}")))
}

fn fill_from_jupiter_quote(
    order: &ApprovedOrder,
    quote: &QuoteResponse,
    mint_decimals: u8,
    dry_run: bool,
    bundle_id: Option<String>,
) -> Fill {
    let (qty, sol_amount, price) = match order.side {
        Side::Buy => {
            let qty = quote.out_amount as f64 / 10f64.powi(mint_decimals as i32);
            let sol_amount = quote.in_amount as f64 / LAMPORTS_PER_SOL;
            let price = if qty > 0.0 { sol_amount / qty } else { 0.0 };
            (qty, sol_amount, price)
        }
        Side::Sell => {
            let qty = quote.in_amount as f64 / 10f64.powi(mint_decimals as i32);
            let sol_amount = quote.out_amount as f64 / LAMPORTS_PER_SOL;
            let price = if qty > 0.0 { sol_amount / qty } else { 0.0 };
            (qty, sol_amount, price)
        }
    };
    Fill {
        mint: order.mint,
        side: order.side,
        qty,
        price,
        sol_amount,
        fee_sol: 0.0, // Jupiter's fee is embedded in the quote; not separately itemized here
        jito_tip_sol: if dry_run { 0.0 } else { JITO_TIP_LAMPORTS as f64 / LAMPORTS_PER_SOL },
        slippage_bps: Some(order.max_slippage_bps),
        strategy: order.strategy.clone(),
        reason: order.reason,
        tx_signature: None,
        bundle_id,
        dry_run,
        ts: order.ts,
    }
}

fn fill_from_bonding_curve_quote(
    order: &ApprovedOrder,
    in_raw: u64,
    out_raw: u64,
    mint_decimals: u8,
    is_buy: bool,
    dry_run: bool,
    bundle_id: Option<String>,
) -> Fill {
    let (qty, sol_amount, price) = if is_buy {
        let qty = out_raw as f64 / 10f64.powi(mint_decimals as i32);
        let sol_amount = in_raw as f64 / LAMPORTS_PER_SOL;
        let price = if qty > 0.0 { sol_amount / qty } else { 0.0 };
        (qty, sol_amount, price)
    } else {
        let qty = in_raw as f64 / 10f64.powi(mint_decimals as i32);
        let sol_amount = out_raw as f64 / LAMPORTS_PER_SOL;
        let price = if qty > 0.0 { sol_amount / qty } else { 0.0 };
        (qty, sol_amount, price)
    };
    Fill {
        mint: order.mint,
        side: order.side,
        qty,
        price,
        sol_amount,
        fee_sol: 0.0,
        jito_tip_sol: if dry_run { 0.0 } else { JITO_TIP_LAMPORTS as f64 / LAMPORTS_PER_SOL },
        slippage_bps: Some(order.max_slippage_bps),
        strategy: order.strategy.clone(),
        reason: order.reason,
        tx_signature: None,
        bundle_id,
        dry_run,
        ts: order.ts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bot_core::{OrderReason, Pubkey, TokenMeta, TrustTier};

    fn order(side: Side, mint: Pubkey, size_sol: f64) -> ApprovedOrder {
        ApprovedOrder {
            side,
            mint,
            size_sol,
            max_slippage_bps: 100,
            reason: OrderReason::Strategy,
            token_meta: TokenMeta {
                mint,
                phase: TokenPhase::BondingCurve,
                trust_tier: TrustTier::BondingCurve,
                discovered_at: 0,
                source: "test".into(),
            },
            strategy: "test".into(),
            ts: 0,
        }
    }

    fn ctx() -> BondingCurveContext {
        BondingCurveContext {
            virtual_token_reserves: 1_000_000_000,
            virtual_sol_reserves: 30_000_000_000,
            creator: Pubkey::new_unique(),
            fee_recipient: Pubkey::new_unique(),
            token_program: pumpfun::TOKEN_PROGRAM_ID.parse().unwrap(),
        }
    }

    #[tokio::test]
    async fn dry_run_bonding_curve_buy_produces_a_real_computed_fill() {
        let exec = Executor::with_rpc_url(true, None);
        let mint = Pubkey::new_unique();
        let ord = order(Side::Buy, mint, 1.0);
        let fill = exec.execute_bonding_curve(&ord, 0.0, 6, ctx(), None).await.unwrap();
        assert!(fill.dry_run);
        assert!(fill.qty > 0.0);
        assert_eq!(fill.side, Side::Buy);
    }

    #[tokio::test]
    async fn live_bonding_curve_without_wallet_is_refused() {
        let exec = Executor::with_rpc_url(false, Some("https://example.invalid".into()));
        let mint = Pubkey::new_unique();
        let ord = order(Side::Buy, mint, 1.0);
        let result = exec.execute_bonding_curve(&ord, 0.0, 6, ctx(), None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn live_bonding_curve_without_rpc_url_is_refused() {
        let exec = Executor::with_rpc_url(false, None);
        let mint = Pubkey::new_unique();
        let wallet = Keypair::new();
        let ord = order(Side::Buy, mint, 1.0);
        let result = exec.execute_bonding_curve(&ord, 0.0, 6, ctx(), Some(&wallet)).await;
        assert!(matches!(result, Err(ExecutionError::UnexpectedResponse(msg)) if msg.contains("RPC URL")));
    }
}
