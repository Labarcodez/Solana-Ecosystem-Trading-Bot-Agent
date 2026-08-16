//! `tui`: the live Ratatui dashboard. `app.rs` holds pure, unit-testable
//! state; `ui.rs` renders it (also tested, via `ratatui::backend::TestBackend`,
//! no real terminal needed); `dashboard.rs` is the async task that ties both
//! to the shared broadcast/watch channels and owns the actual terminal.

pub mod app;
pub mod dashboard;
pub mod error;
pub mod ui;

pub use app::App;
pub use dashboard::{run, ControlCommand, DashboardChannels};
pub use error::TuiError;
