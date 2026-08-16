//! Ratatui layout: a status bar, a price chart, a positions table + P&L
//! summary, and a scrolling event log. Pure rendering - reads `App`, never
//! mutates it, and never blocks (no I/O here at all), so a slow terminal
//! redraw can only make this task's own view lag, never the trading loop
//! (see `dashboard.rs`).

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, Borders, Chart, Dataset, GraphType, List, ListItem, Paragraph, Row, Table};
use ratatui::Frame;

use crate::app::{short_mint, App};

pub fn render(frame: &mut Frame, app: &App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(10), Constraint::Length(3)])
        .split(frame.area());

    render_status_bar(frame, root[0], app);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(root[1]);

    render_price_chart(frame, body[0], app);
    render_positions_panel(frame, body[1], app);

    render_event_log(frame, root[2], app);
}

fn render_status_bar(frame: &mut Frame, area: Rect, app: &App) {
    let breaker = if app.snapshot.circuit_breaker_tripped { "TRIPPED" } else { "normal" };
    let breaker_style = if app.snapshot.circuit_breaker_tripped {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Green)
    };
    let paused = if app.paused { " | PAUSED" } else { "" };

    let line = Line::from(vec![
        Span::styled(format!(" {} ", app.mode.to_uppercase()), Style::default().fg(Color::Yellow)),
        Span::raw("| "),
        Span::raw(format!("strategy: {} ", app.strategy)),
        Span::raw("| "),
        Span::raw(format!("equity: {:.4} SOL ", app.snapshot.equity_sol)),
        Span::raw("| "),
        Span::raw(format!("daily PnL: {:+.4} SOL ", app.snapshot.daily_pnl_sol)),
        Span::raw("| "),
        Span::styled(format!("breaker: {breaker}"), breaker_style),
        Span::raw(paused),
    ]);
    frame.render_widget(Paragraph::new(line).block(Block::default().borders(Borders::ALL).title(" trading-bot ")), area);
}

fn render_price_chart(frame: &mut Frame, area: Rect, app: &App) {
    let points: Vec<(f64, f64)> = app
        .price_history
        .iter()
        .enumerate()
        .map(|(i, (_, price))| (i as f64, *price))
        .collect();

    let (min_y, max_y) = points.iter().fold((f64::MAX, f64::MIN), |(lo, hi), (_, y)| (lo.min(*y), hi.max(*y)));
    let (min_y, max_y) = if points.is_empty() { (0.0, 1.0) } else { (min_y, max_y) };
    let pad = ((max_y - min_y) * 0.05).max(1e-9);

    let dataset = Dataset::default()
        .name("price")
        .marker(symbols::Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::default().fg(Color::Cyan))
        .data(&points);

    let chart = Chart::new(vec![dataset])
        .block(Block::default().borders(Borders::ALL).title(" price "))
        .x_axis(Axis::default().bounds([0.0, points.len().max(1) as f64]))
        .y_axis(
            Axis::default()
                .bounds([min_y - pad, max_y + pad])
                .labels(vec![format!("{min_y:.6}"), format!("{max_y:.6}")]),
        );
    frame.render_widget(chart, area);
}

fn render_positions_panel(frame: &mut Frame, area: Rect, app: &App) {
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(6)])
        .split(area);

    let rows: Vec<Row> = app
        .positions
        .values()
        .map(|p| {
            let current = app.last_price.get(&p.mint).copied().unwrap_or(p.entry_price);
            let pnl_pct = if p.entry_price > 0.0 { (current - p.entry_price) / p.entry_price * 100.0 } else { 0.0 };
            let style = if pnl_pct >= 0.0 { Style::default().fg(Color::Green) } else { Style::default().fg(Color::Red) };
            Row::new(vec![
                short_mint(&p.mint),
                p.strategy.clone(),
                format!("{:.6}", p.entry_price),
                format!("{:.4}", p.qty),
                format!("{pnl_pct:+.2}%"),
            ])
            .style(style)
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(11),
            Constraint::Length(10),
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Length(9),
        ],
    )
    .header(Row::new(vec!["mint", "strategy", "entry", "qty", "pnl%"]).style(Style::default().add_modifier(Modifier::BOLD)))
    .block(Block::default().borders(Borders::ALL).title(format!(" positions ({}) ", app.positions.len())));
    frame.render_widget(table, sections[0]);

    let summary = Paragraph::new(vec![
        Line::from(format!("realized:   {:+.4} SOL", app.snapshot.realized_pnl_sol)),
        Line::from(format!("unrealized: {:+.4} SOL", app.snapshot.unrealized_pnl_sol)),
        Line::from(format!("trades:     {}", app.trade_count)),
    ])
    .block(Block::default().borders(Borders::ALL).title(" pnl "));
    frame.render_widget(summary, sections[1]);
}

fn render_event_log(frame: &mut Frame, area: Rect, app: &App) {
    let visible = area.height.saturating_sub(2) as usize;
    let items: Vec<ListItem> = app
        .events
        .iter()
        .rev()
        .take(visible.max(1))
        .rev()
        .map(|e| ListItem::new(e.as_str()))
        .collect();
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(" events (q: quit, p: pause) "));
    frame.render_widget(list, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use bot_core::{PriceTick, Pubkey};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// Flattens the rendered buffer to a single string so tests can assert
    /// on substrings without hand-building an exact expected `Buffer`.
    fn rendered_text(app: &App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut out = String::new();
        for cell in buffer.content() {
            out.push_str(cell.symbol());
        }
        out
    }

    #[test]
    fn status_bar_shows_mode_and_strategy() {
        let app = App::new("dry_run", "momentum");
        let text = rendered_text(&app, 100, 30);
        assert!(text.contains("DRY_RUN"));
        assert!(text.contains("momentum"));
    }

    #[test]
    fn tripped_breaker_is_visible_in_the_rendered_output() {
        let mut app = App::new("dry_run", "grid");
        app.on_snapshot(bot_core::Snapshot { circuit_breaker_tripped: true, ..Default::default() });
        let text = rendered_text(&app, 100, 30);
        assert!(text.contains("TRIPPED"));
    }

    #[test]
    fn renders_without_panicking_on_an_empty_app() {
        let app = App::new("dry_run", "momentum");
        let _ = rendered_text(&app, 80, 24);
    }

    #[test]
    fn renders_without_panicking_with_price_history_and_positions() {
        let mut app = App::new("dry_run", "momentum");
        let mint = Pubkey::new_unique();
        for i in 0..50 {
            app.on_price_tick(&PriceTick { mint, price: 1.0 + (i as f64) * 0.01, ts: i });
        }
        app.on_app_event(&bot_core::AppEvent::Fill(bot_core::Fill {
            mint, side: bot_core::Side::Buy, qty: 5.0, price: 1.2, sol_amount: 6.0,
            fee_sol: 0.0, jito_tip_sol: 0.0, slippage_bps: None,
            strategy: "momentum".into(), reason: bot_core::OrderReason::Strategy,
            tx_signature: None, bundle_id: None, dry_run: true, ts: 50,
        }));
        let text = rendered_text(&app, 120, 40);
        assert!(text.contains("momentum"));
    }

    #[test]
    fn event_log_shows_the_most_recent_entries() {
        let mut app = App::new("dry_run", "momentum");
        for i in 0..300 {
            app.on_app_event(&bot_core::AppEvent::Log {
                level: bot_core::LogLevel::Info,
                message: format!("evt{i}"),
                ts: i,
            });
        }
        let text = rendered_text(&app, 100, 30);
        // The very last event should be visible somewhere in the log panel.
        assert!(text.contains("evt299"));
    }
}
