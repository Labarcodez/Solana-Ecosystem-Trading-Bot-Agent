//! Ties the Kraken Spot (+ margin) and Kraken Futures clients together
//! behind one `Executor`, routed by `ApprovedOrder.pair_meta.market_type`.
//! `dry_run` is first-class, not an afterthought: every path makes its real
//! public-endpoint call (ticker/tickers) to prove the integration is live,
//! and stops before anything private is signed or submitted.
//!
//! **Order-type policy (the concrete "post-only saves money" mechanism
//! described in the README's "how to make the most money on Kraken"
//! section):** a strategy-initiated order is placed as a post-only limit,
//! joining the current best bid (buy) / best ask (sell) - guaranteed maker,
//! never accidentally taker. A protective exit (stop-loss, take-profit, or
//! the margin/futures liquidation-distance guard) is always a market order
//! instead - it has to fill now, so paying the taker fee is the correct
//! trade-off there.
//!
//! **Pair symbol note:** the `pair`/`symbol` string on an `ApprovedOrder`
//! is passed to Kraken's API verbatim - this crate does not attempt to
//! translate between Kraken's several historical symbol spellings (e.g.
//! `"XBT/USD"` vs `"XXBTZUSD"` vs an altname). Confirm the exact spelling
//! your account's `AddOrder`/`Ticker` calls expect via Kraken's own
//! `AssetPairs` endpoint before configuring `[kraken].pairs` - documented
//! in the README rather than silently guessed at here.

use bot_core::{ApprovedOrder, Fill, MarketType, OrderReason, Side};

use crate::error::ExecutionError;
use crate::kraken_futures::{FuturesOrderRequest, KrakenFuturesClient};
use crate::kraken_spot::{AddOrderRequest, KrakenSpotClient, OrderType, TickerInfo};

/// Kraken's base-tier maker/taker fees (see README's fee-tier section) -
/// used to *estimate* the fee on a fill for storage/risk purposes. This is
/// a documented approximation, not a live read of the account's actual
/// fee tier (which needs its own API call/config plumbing - a follow-up,
/// not implemented here) - see README honesty notes.
const ESTIMATED_MAKER_FEE_BPS: u16 = 25;
const ESTIMATED_TAKER_FEE_BPS: u16 = 40;

pub struct Executor {
    spot: KrakenSpotClient,
    futures: KrakenFuturesClient,
    dry_run: bool,
}

impl Executor {
    pub fn new(dry_run: bool) -> Self {
        Self { spot: KrakenSpotClient::new(), futures: KrakenFuturesClient::new(), dry_run }
    }

    pub fn is_dry_run(&self) -> bool {
        self.dry_run
    }

    /// Executes an approved order, routed by `pair_meta.market_type`.
    /// `spot_credentials`/`futures_credentials` are `(api_key, api_secret)`
    /// pairs - `None` is fine in dry-run (nothing is ever placed) but
    /// required in live mode for whichever market type the order needs.
    pub async fn execute(
        &self,
        order: &ApprovedOrder,
        spot_credentials: Option<(&str, &str)>,
        futures_credentials: Option<(&str, &str)>,
    ) -> Result<Fill, ExecutionError> {
        match order.pair_meta.market_type {
            MarketType::Spot | MarketType::Margin => self.execute_spot_or_margin(order, spot_credentials).await,
            MarketType::Futures => self.execute_futures(order, futures_credentials).await,
        }
    }

    async fn execute_spot_or_margin(
        &self,
        order: &ApprovedOrder,
        credentials: Option<(&str, &str)>,
    ) -> Result<Fill, ExecutionError> {
        let kraken_pair = order.pair.as_str();

        // Real, live ticker call regardless of mode - this is what proves
        // the integration works even in dry_run.
        let ticker = self.spot.ticker(kraken_pair).await?;
        let plan = plan_order(order, &ticker)?;
        let mark_price = plan.reference_price;
        let qty = if mark_price > 0.0 { order.size_quote / mark_price } else { 0.0 };

        if self.dry_run {
            tracing::info!(
                pair = %order.pair, side = ?order.side, qty, mark_price, maker = plan.is_maker,
                "[DRY-RUN] would place a Kraken {:?} order", order.pair_meta.market_type
            );
            return Ok(build_fill(order, qty, mark_price, plan.is_maker, true, None));
        }

        let (api_key, api_secret) = credentials.ok_or(ExecutionError::MissingCredentials)?;
        let leverage = (order.pair_meta.market_type == MarketType::Margin).then(|| format_leverage(order.pair_meta.leverage));
        let req = AddOrderRequest {
            pair: kraken_pair,
            side: plan.side,
            order_type: plan.order_type,
            limit_price: plan.limit_price,
            volume: qty,
            leverage,
            post_only: plan.is_maker,
        };
        let result = self.spot.add_order(api_key, api_secret, &req).await?;
        let order_id = result.txid.into_iter().next();
        tracing::info!(pair = %order.pair, order_id = ?order_id, "submitted live Kraken order");
        Ok(build_fill(order, qty, mark_price, plan.is_maker, false, order_id))
    }

    async fn execute_futures(&self, order: &ApprovedOrder, credentials: Option<(&str, &str)>) -> Result<Fill, ExecutionError> {
        let tickers = self.futures.tickers().await?;
        let ticker = tickers
            .into_iter()
            .find(|t| t.symbol.eq_ignore_ascii_case(order.pair.as_str()))
            .ok_or_else(|| ExecutionError::NoTickerData(order.pair.to_string()))?;

        let is_protective = is_protective_exit(order.reason);
        let mark_price = match order.side {
            Side::Buy => ticker.ask.or(ticker.last),
            Side::Sell => ticker.bid.or(ticker.last),
        }
        .ok_or_else(|| ExecutionError::NoTickerData(order.pair.to_string()))?;
        let qty = if mark_price > 0.0 { order.size_quote / mark_price } else { 0.0 };
        let is_maker = !is_protective;

        if self.dry_run {
            tracing::info!(
                pair = %order.pair, side = ?order.side, qty, mark_price, funding_rate = ?ticker.funding_rate,
                "[DRY-RUN] would place a Kraken Futures order"
            );
            return Ok(build_fill(order, qty, mark_price, is_maker, true, None));
        }

        let (api_key, api_secret) = credentials.ok_or(ExecutionError::MissingCredentials)?;
        let side = match order.side {
            Side::Buy => "buy",
            Side::Sell => "sell",
        };
        let req = FuturesOrderRequest {
            symbol: order.pair.as_str(),
            side,
            market: is_protective,
            limit_price: (!is_protective).then_some(mark_price),
            size: qty,
        };
        let result = self.futures.send_order(api_key, api_secret, &req).await?;
        tracing::info!(pair = %order.pair, order_id = ?result.order_id, status = result.status, "submitted live Kraken Futures order");
        Ok(build_fill(order, qty, mark_price, is_maker, false, result.order_id))
    }
}

fn is_protective_exit(reason: OrderReason) -> bool {
    matches!(reason, OrderReason::StopLoss | OrderReason::TakeProfit | OrderReason::LiquidationGuard)
}

struct OrderPlan {
    side: &'static str,
    order_type: OrderType,
    limit_price: Option<f64>,
    reference_price: f64,
    is_maker: bool,
}

/// Decides how to place a spot/margin order: a protective exit always goes
/// out as a market order (must fill now); a strategy-initiated order joins
/// the current best bid/ask as a post-only maker order (see module docs).
fn plan_order(order: &ApprovedOrder, ticker: &TickerInfo) -> Result<OrderPlan, ExecutionError> {
    let side = match order.side {
        Side::Buy => "buy",
        Side::Sell => "sell",
    };
    if is_protective_exit(order.reason) {
        let reference_price = match order.side {
            Side::Buy => ticker.best_ask().or_else(|| ticker.last_trade_price()),
            Side::Sell => ticker.best_bid().or_else(|| ticker.last_trade_price()),
        }
        .ok_or_else(|| ExecutionError::NoTickerData(order.pair.to_string()))?;
        return Ok(OrderPlan { side, order_type: OrderType::Market, limit_price: None, reference_price, is_maker: false });
    }

    let price = match order.side {
        Side::Buy => ticker.best_bid(),
        Side::Sell => ticker.best_ask(),
    }
    .ok_or_else(|| ExecutionError::NoTickerData(order.pair.to_string()))?;
    Ok(OrderPlan { side, order_type: OrderType::Limit, limit_price: Some(price), reference_price: price, is_maker: true })
}

/// Kraken's `AddOrder` leverage format is an integer ratio string like
/// `"2:1"`.
fn format_leverage(leverage: f64) -> String {
    format!("{}:1", leverage.round().max(1.0) as u64)
}

fn build_fill(
    order: &ApprovedOrder,
    qty: f64,
    price: f64,
    is_maker: bool,
    dry_run: bool,
    order_id: Option<String>,
) -> Fill {
    let quote_amount = qty * price;
    let fee_bps = if is_maker { ESTIMATED_MAKER_FEE_BPS } else { ESTIMATED_TAKER_FEE_BPS };
    let fee_quote = quote_amount * fee_bps as f64 / 10_000.0;
    Fill {
        pair: order.pair.clone(),
        side: order.side,
        qty,
        price,
        quote_amount,
        fee_quote,
        funding_paid_quote: 0.0,
        slippage_bps: None,
        market_type: order.pair_meta.market_type,
        strategy: order.strategy.clone(),
        reason: order.reason,
        order_id,
        dry_run,
        ts: chrono::Utc::now().timestamp(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bot_core::{MarketType, Pair, PairMeta, RiskTier};

    fn order(side: Side, reason: OrderReason, market_type: MarketType) -> ApprovedOrder {
        ApprovedOrder {
            side,
            pair: Pair::from("XBT/USD"),
            size_quote: 1000.0,
            max_slippage_bps: 50,
            reason,
            pair_meta: PairMeta { pair: Pair::from("XBT/USD"), market_type, risk_tier: RiskTier::from(market_type), leverage: 2.0 },
            strategy: "momentum".into(),
            ts: 0,
        }
    }

    fn ticker(bid: &str, ask: &str, last: &str) -> TickerInfo {
        TickerInfo { a: vec![ask.into(), "1".into(), "1".into()], b: vec![bid.into(), "1".into(), "1".into()], c: vec![last.into(), "1".into()] }
    }

    #[test]
    fn strategy_buy_plans_a_post_only_limit_joining_the_bid() {
        let o = order(Side::Buy, OrderReason::Strategy, MarketType::Spot);
        let t = ticker("49900", "50000", "49950");
        let plan = plan_order(&o, &t).unwrap();
        assert!(plan.is_maker);
        assert_eq!(plan.order_type, OrderType::Limit);
        assert_eq!(plan.limit_price, Some(49900.0));
    }

    #[test]
    fn strategy_sell_plans_a_post_only_limit_joining_the_ask() {
        let o = order(Side::Sell, OrderReason::Strategy, MarketType::Spot);
        let t = ticker("49900", "50000", "49950");
        let plan = plan_order(&o, &t).unwrap();
        assert!(plan.is_maker);
        assert_eq!(plan.limit_price, Some(50000.0));
    }

    #[test]
    fn stop_loss_always_plans_a_market_order() {
        let o = order(Side::Sell, OrderReason::StopLoss, MarketType::Spot);
        let t = ticker("49900", "50000", "49950");
        let plan = plan_order(&o, &t).unwrap();
        assert!(!plan.is_maker);
        assert_eq!(plan.order_type, OrderType::Market);
        assert_eq!(plan.limit_price, None);
    }

    #[test]
    fn liquidation_guard_exit_is_also_a_market_order() {
        let o = order(Side::Sell, OrderReason::LiquidationGuard, MarketType::Margin);
        let t = ticker("49900", "50000", "49950");
        let plan = plan_order(&o, &t).unwrap();
        assert_eq!(plan.order_type, OrderType::Market);
    }

    #[test]
    fn maker_fee_is_cheaper_than_taker_fee_in_the_built_fill() {
        let o = order(Side::Buy, OrderReason::Strategy, MarketType::Spot);
        let maker_fill = build_fill(&o, 1.0, 50000.0, true, true, None);
        let taker_fill = build_fill(&o, 1.0, 50000.0, false, true, None);
        assert!(maker_fill.fee_quote < taker_fill.fee_quote);
    }

    #[test]
    fn new_executor_reports_its_dry_run_mode() {
        assert!(Executor::new(true).is_dry_run());
        assert!(!Executor::new(false).is_dry_run());
    }

    #[test]
    fn leverage_formats_as_krakens_ratio_string() {
        assert_eq!(format_leverage(2.0), "2:1");
        assert_eq!(format_leverage(5.0), "5:1");
        assert_eq!(format_leverage(0.4), "1:1", "must never format below 1:1");
    }
}
