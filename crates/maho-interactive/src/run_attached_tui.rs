//! Port of senpi `modes/interactive/run-attached-tui.ts`: the presentation boundary of `--attach`,
//! parallel to the ordinary interactive mode. It connects before the terminal is taken over, so a
//! refusal or a connection failure is one plain error line, then runs the frame loop of the other
//! standalone startup screens, and releases the attachment after the terminal is restored.
use crate::{
    attach::{AttachedTui, validate_attach_terminal},
    attach_dialogs::AttachEditorHost,
    theme::theme::Theme,
    tui_renderer::{
        InteractiveTui, InteractiveTuiOptions, TuiMode, create_interactive_tui,
        create_process_terminal,
    },
};
use maho_tui::{terminal::Terminal, tui::Component};
use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, atomic::AtomicBool, atomic::Ordering},
};

pub struct RunAttachedTuiOptions {
    pub address: String,
    pub session: String,
    pub tui_mode: TuiMode,
    pub theme: Theme,
    pub agent_dir: PathBuf,
    /// Set by a signal handler: the view leaves at the next frame.
    pub interrupted: Arc<AtomicBool>,
}

pub fn run_attached_tui(options: RunAttachedTuiOptions) -> Result<(), String> {
    use std::io::IsTerminal;
    validate_attach_terminal(
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
    )?;
    let agent_dir = options.agent_dir.to_string_lossy().into_owned();
    let manager = maho_core::keybindings::KeybindingsManager::create(Some(&agent_dir))
        .inner()
        .clone();
    maho_tui::keybindings::set_keybindings(manager.clone());
    let mut terminal = create_process_terminal();
    let host = Rc::new(AttachEditorHost::new(usize::from(terminal.rows())));
    let view = Rc::new(RefCell::new(AttachedTui::new(
        &options.theme,
        Arc::new(manager),
        host.clone(),
        None,
    )));
    view.borrow_mut().connect(
        &options.address,
        &options.session,
        usize::from(terminal.columns()),
        Some(options.agent_dir.as_path()),
    )?;
    let mut screen = create_interactive_tui(
        InteractiveTuiOptions {
            tui_mode: options.tui_mode,
            show_hardware_cursor: false,
            bottom_shortcut: String::new(),
        },
        options.theme.clone(),
    );
    screen
        .base_mut()
        .set_log_directory(options.agent_dir.clone());
    let root: Rc<RefCell<dyn Component>> = match options.tui_mode {
        TuiMode::Regular => view.clone(),
        TuiMode::Fullscreen => view.borrow().layout_root(),
    };
    screen.set_render_root(root);
    let focus: Rc<RefCell<dyn Component>> = view.clone();
    screen.base_mut().set_focus(Some(focus));
    let input = Rc::new(RefCell::new(Vec::<String>::new()));
    let captured = input.clone();
    screen.base_mut().prepare_start();
    screen.before_terminal_start(&mut terminal, false, false);
    terminal.start(
        Box::new(move |chunk| captured.borrow_mut().push(chunk.into())),
        Box::new(|| {}),
    );
    screen.base_mut().after_terminal_start(&mut terminal);
    let clock = std::time::Instant::now();
    let result = (|| {
        while !options.interrupted.load(Ordering::SeqCst) && !view.borrow().finished() {
            let now =
                u64::try_from(clock.elapsed().as_millis()).map_err(|error| error.to_string())?;
            for chunk in std::mem::take(&mut *input.borrow_mut()) {
                route_input(&mut screen, &mut terminal, &chunk, now);
            }
            host.set_rows(usize::from(terminal.rows()));
            view.borrow_mut().poll();
            screen.base_mut().request_render(false, now);
            screen.do_render(&mut terminal);
            terminal.pump(16).map_err(|error| error.to_string())?;
        }
        Ok::<(), String>(())
    })();
    screen.base_mut().begin_stop(&mut terminal);
    screen.before_terminal_stop(&mut terminal, false);
    screen.base_mut().finish_stop_output(&mut terminal, false);
    let stopped = terminal.stop().map_err(|error| error.to_string());
    screen.after_terminal_stop(&mut terminal, false);
    screen.base_mut().reset_for_stop();
    let released = view.borrow_mut().close();
    result.and(stopped)?;
    released
}

/// The terminal's protocol replies are consumed, then mouse input and (fullscreen) the viewport keys,
/// and the rest goes to the focused view.
fn route_input(screen: &mut InteractiveTui, terminal: &mut dyn Terminal, chunk: &str, now_ms: u64) {
    let Some(data) = screen.base_mut().consume_terminal_protocol_input(chunk) else {
        return;
    };
    if screen.handle_mouse_input(&data, i64::try_from(now_ms).unwrap_or(i64::MAX), terminal) {
        return;
    }
    let released = maho_tui::keys::is_key_release(&data);
    if let InteractiveTui::Fullscreen(alt) = screen
        && alt.handle_viewport_input(&data, released)
    {
        return;
    }
    screen.base_mut().apply_input_focus_rules();
    screen.base_mut().handle_terminal_input(&data, released);
}
