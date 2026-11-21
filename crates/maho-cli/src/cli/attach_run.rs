//! The `--attach` dispatch of senpi `main.ts`: run the attached TUI against an existing RPC endpoint.
use maho_interactive::{
    run_attached_tui::{RunAttachedTuiOptions, run_attached_tui},
    tui_renderer::TuiMode,
};
use std::sync::{Arc, atomic::AtomicBool};

/// A SIGINT or SIGTERM ends the view at its next frame instead of leaving the terminal in raw mode.
#[cfg(unix)]
fn leave_on_signal(interrupted: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        runtime.block_on(async {
            use tokio::signal::unix::{SignalKind, signal};
            let (Ok(mut interrupt), Ok(mut terminate)) = (
                signal(SignalKind::interrupt()),
                signal(SignalKind::terminate()),
            ) else {
                return;
            };
            tokio::select! {
                _ = interrupt.recv() => {}
                _ = terminate.recv() => {}
            }
            interrupted.store(true, std::sync::atomic::Ordering::SeqCst);
        });
    });
}

#[cfg(not(unix))]
fn leave_on_signal(_interrupted: Arc<AtomicBool>) {}

/// `--tui-mode fullscreen` selects the alternate screen; anything else is the regular screen.
pub fn run_attach(
    address: &str,
    session: &str,
    tui_mode: Option<&str>,
    theme: Option<&str>,
) -> Result<(), String> {
    let theme = super::startup_ui::resolve_startup_theme(
        theme,
        std::env::var("COLORFGBG").ok().as_deref(),
    )?;
    let interrupted = Arc::new(AtomicBool::new(false));
    leave_on_signal(interrupted.clone());
    run_attached_tui(RunAttachedTuiOptions {
        address: address.to_owned(),
        session: session.to_owned(),
        tui_mode: if tui_mode == Some("fullscreen") {
            TuiMode::Fullscreen
        } else {
            TuiMode::Regular
        },
        theme,
        agent_dir: std::path::PathBuf::from(maho_core::config::get_agent_dir()),
        interrupted,
    })
}
