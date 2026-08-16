//! Ties the Jupiter, Jito, and pump.fun clients together behind one
//! `Executor`, routed by `ApprovedOrder.token_meta.phase`. `dry_run` is
//! first-class, not an afterthought: both paths make their real quote call
//! (proving the integration is live) and stop before anything is signed or
//! submitted.

use std::str::FromStr;

use bot_core::{ApprovedOrder, Fill, Side, TokenPhase};
use solana_sdk::signature::{Keypair, Signer};
use solana_sdk::transaction::VersionedTransaction;
use solana_system_interface::instruction as system_instruction;

use crate::error::ExecutionError;
use crate::jito::JitoClient;
use crate::jupiter::{JupiterClient, QuoteResponse};
use crate::pumpfun::{self, PumpFunAccounts};

const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";
const LAMPORTS_PER_SOL: f64 = 1_000_000_000.0;

/// Everything about the bonding curve's current state the executor needs
/// for a pump.fun quote/trade - supplied by the caller (from
/// `market_data`/`discovery`'s last-known reading or a fresh RPC fetch)
/// rather than fetched internally, so `Executor` doesn't need its own RPC
/// client just for this.
#[derive(Debug, Clone, Copy)]
pub struct BondingCurveContext {
    pub virtual_token_reserves: u64,
    pub virtual_sol_reserves: u64,
    pub creator: bot_core::Pubkey,
}

pub struct Executor {
    jupiter: JupiterClient,
    jito: JitoClient,
    dry_run: bool,
    wsol_mint: bot_core::Pubkey,
}

impl Executor {
    pub fn new(dry_run: bool) -> Self {
        Self {
            jupiter: JupiterClient::new(),
            jito: JitoClient::new(),
            dry_run,
            wsol_mint: bot_core::Pubkey::from_str(WSOL_MINT).expect("valid static mint"),
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
                self.execute_bonding_curve(order, mark_price, mint_decimals, ctx, wallet)
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
        let bundle_id = self.sign_and_submit_via_jito(swap_tx_b64, wallet).await?;
        tracing::info!(mint = %order.mint, bundle_id, "submitted live swap via Jito bundle");
        Ok(fill_from_jupiter_quote(order, &quote, mint_decimals, false, Some(bundle_id)))
    }

    fn execute_bonding_curve(
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

        let Some(_wallet) = wallet else {
            return Err(ExecutionError::UnexpectedResponse("live execution requires an unlocked wallet".into()));
        };
        // Building + submitting a live bonding-curve instruction is
        // deliberately not wired further than this in the current build -
        // `PumpFunAccounts`' account list needs the live-transaction
        // verification called out in `pumpfun.rs` before it's trusted with
        // real funds. `mode = "live"` on a bonding-curve token is rejected
        // here rather than silently attempting an unverified instruction.
        Err(ExecutionError::UnexpectedResponse(
            "live bonding-curve trading requires verifying PumpFunAccounts against a real transaction first (see pumpfun.rs docs) - not enabled in this build".into(),
        ))
    }

    /// Signs `swap_tx_b64` with `wallet`, wraps it with a Jito tip transfer,
    /// and submits the bundle. Real, complete code - exercised only when
    /// the user runs `mode = "live"` with real funds, matching the user's
    /// own stated cost model (gas/tips are the only real cost).
    async fn sign_and_submit_via_jito(&self, swap_tx_b64: String, wallet: &Keypair) -> Result<String, ExecutionError> {
        let tx_bytes = base64_decode(&swap_tx_b64)?;
        let versioned_tx: VersionedTransaction =
            bincode::deserialize(&tx_bytes).map_err(|e| ExecutionError::UnexpectedResponse(e.to_string()))?;
        let signed = sign_versioned_transaction(versioned_tx, wallet)?;

        let tip_accounts = self.jito.get_tip_accounts().await?;
        let tip_account = tip_accounts
            .first()
            .copied()
            .ok_or_else(|| ExecutionError::Jito("no tip accounts returned".into()))?;
        let tip_ix = system_instruction::transfer(&wallet.pubkey(), &tip_account, 10_000);
        let recent_blockhash = signed.message.recent_blockhash().to_owned();
        let tip_tx = solana_sdk::transaction::Transaction::new_signed_with_payer(
            &[tip_ix],
            Some(&wallet.pubkey()),
            &[wallet],
            recent_blockhash,
        );

        let signed_txs = vec![
            bs58::encode(bincode::serialize(&tip_tx).unwrap()).into_string(),
            bs58::encode(bincode::serialize(&signed).unwrap()).into_string(),
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
        jito_tip_sol: if dry_run { 0.0 } else { 10_000.0 / LAMPORTS_PER_SOL },
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
        jito_tip_sol: if dry_run { 0.0 } else { 10_000.0 / LAMPORTS_PER_SOL },
        slippage_bps: Some(order.max_slippage_bps),
        strategy: order.strategy.clone(),
        reason: order.reason,
        tx_signature: None,
        bundle_id,
        dry_run,
        ts: order.ts,
    }
}

/// Referenced so `PumpFunAccounts`/`BondingCurveContext` stay linked for
/// callers assembling a full live bonding-curve trade later.
pub fn pumpfun_accounts_for(order: &ApprovedOrder, wallet: bot_core::Pubkey, ctx: &BondingCurveContext) -> PumpFunAccounts {
    let fee_recipient = ctx.creator; // best-effort placeholder - see pumpfun.rs verification note
    PumpFunAccounts::derive(order.mint, wallet, ctx.creator, fee_recipient)
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

    #[test]
    fn dry_run_bonding_curve_buy_produces_a_real_computed_fill() {
        let exec = Executor::new(true);
        let mint = Pubkey::new_unique();
        let ctx = BondingCurveContext {
            virtual_token_reserves: 1_000_000_000,
            virtual_sol_reserves: 30_000_000_000,
            creator: Pubkey::new_unique(),
        };
        let ord = order(Side::Buy, mint, 1.0);
        let fill = exec.execute_bonding_curve(&ord, 0.0, 6, ctx, None).unwrap();
        assert!(fill.dry_run);
        assert!(fill.qty > 0.0);
        assert_eq!(fill.side, Side::Buy);
    }

    #[test]
    fn live_bonding_curve_without_verified_accounts_is_refused() {
        let exec = Executor::new(false);
        let mint = Pubkey::new_unique();
        let ctx = BondingCurveContext {
            virtual_token_reserves: 1_000_000_000,
            virtual_sol_reserves: 30_000_000_000,
            creator: Pubkey::new_unique(),
        };
        let wallet = Keypair::new();
        let ord = order(Side::Buy, mint, 1.0);
        let result = exec.execute_bonding_curve(&ord, 0.0, 6, ctx, Some(&wallet));
        assert!(result.is_err());
    }
}
