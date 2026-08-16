//! The dashboard task: owns the terminal, subscribes to the shared
//! broadcast buses (read-only), and redraws on a fixed interval. It never
//! calls into `strategy`/`risk`/`execution` and never blocks anything else -
//! a slow terminal redraw can only make its own view lag, structurally
//! unable to backpressure the trading loop, since it only ever reads.

use std::io::Stdout;
use std::time::Duration;

use bot_core::{AppEvent, PriceTick, Snapshot};
use crossterm::event::{Event, EventStream, KeyCode};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::{broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::app::App;
use crate::error::TuiError;
use crate::ui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlCommand {
    Quit,
    TogglePause,
}

pub struct DashboardChannels {
    pub price_rx: broadcast::Receiver<PriceTick>,
    pub event_rx: broadcast::Receiver<AppEvent>,
    pub snapshot_rx: watch::Receiver<Snapshot>,
}

/// Runs until `shutdown` is cancelled, the user presses `q`, or the price
/// bus closes. Sends `ControlCommand`s back so `bin/trading-bot` can react
/// (e.g. a future pause implementation could gate signal evaluation).
pub async fn run(
    mode: String,
    strategy: String,
    mut channels: DashboardChannels,
    control_tx: mpsc::UnboundedSender<ControlCommand>,
    shutdown: CancellationToken,
) -> Result<(), TuiError> {
    let mut terminal = enter_terminal()?;
    let mut app = App::new(mode, strategy);
    let mut redraw = tokio::time::interval(Duration::from_millis(150));
    let mut key_events = EventStream::new();

    let result: Result<(), TuiError> = loop {
        tokio::select! {
            _ = shutdown.cancelled() => break Ok(()),
            _ = redraw.tick() => {
                if let Err(e) = terminal.draw(|f| ui::render(f, &app)) {
                    break Err(e.into());
                }
            }
            tick = channels.price_rx.recv() => match tick {
                Ok(t) => app.on_price_tick(&t),
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break Ok(()),
            },
            ev = channels.event_rx.recv() => match ev {
                Ok(e) => app.on_app_event(&e),
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => {}
            },
            changed = channels.snapshot_rx.changed() => {
                if changed.is_ok() {
                    app.on_snapshot(channels.snapshot_rx.borrow_and_update().clone());
                }
            }
            maybe_event = key_events.next() => {
                if let Some(Ok(Event::Key(key))) = maybe_event {
                    match key.code {
                        KeyCode::Char('q') => {
                            let _ = control_tx.send(ControlCommand::Quit);
                            break Ok(());
                        }
                        KeyCode::Char('p') => {
                            app.toggle_pause();
                            let _ = control_tx.send(ControlCommand::TogglePause);
                        }
                        _ => {}
                    }
                }
            }
        }
    };

    leave_terminal(&mut terminal)?;
    result
}

fn enter_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>, TuiError> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn leave_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<(), TuiError> {
    disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;
    Ok(())
}
