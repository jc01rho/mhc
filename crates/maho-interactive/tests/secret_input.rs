use maho_ext_api::{ExtensionUi, ExtensionUiDialogOptions, TerminalInputResult};
use maho_interactive::{
    components::extension_input::{ExtensionInputComponent, ExtensionInputOptions},
    theme::{ColorMode, Theme},
};
use maho_tui::{tui::Component, utils::strip_terminal_sequences};
use std::{cell::RefCell, rc::Rc, sync::Arc};

fn dark_theme() -> Theme {
    Theme::builtin("dark", ColorMode::Truecolor).expect("dark theme")
}

#[test]
fn extension_input_masks_secret_and_scrubs_state_on_dispose() {
    let theme = dark_theme();
    let submitted = Rc::new(RefCell::new(None::<String>));
    let submit_sink = Rc::clone(&submitted);
    let mut component = ExtensionInputComponent::new(
        &theme,
        "Credential",
        Box::new(move |value| *submit_sink.borrow_mut() = Some(value.to_owned())),
        Box::new(|| {}),
        ExtensionInputOptions {
            secret: true,
            ..Default::default()
        },
    );
    let sentinel = "synthetic-secret-token";
    component.handle_input(sentinel);
    let rendered = strip_terminal_sequences(&component.render(80).join("\n"));
    assert!(!rendered.contains(sentinel));
    assert!(rendered.contains("**********************"));

    component.handle_input("\n");
    assert_eq!(submitted.borrow().as_deref(), Some(sentinel));

    component.dispose();
    let after_dispose = strip_terminal_sequences(&component.render(80).join("\n"));
    assert!(!after_dispose.contains(sentinel));
    assert!(!after_dispose.contains('*'));
}

#[test]
fn extension_input_rejects_multiline_paste_and_destroys_kill_and_undo_buffers() {
    let theme = dark_theme();
    let submitted = Rc::new(RefCell::new(Vec::<String>::new()));
    let submit_sink = Rc::clone(&submitted);
    let mut component = ExtensionInputComponent::new(
        &theme,
        "Private",
        Box::new(move |value| submit_sink.borrow_mut().push(value.to_owned())),
        Box::new(|| {}),
        ExtensionInputOptions {
            secret: true,
            ..Default::default()
        },
    );
    component.handle_input("\x1b[200~line\nbreak\x1b[201~");
    let warning = strip_terminal_sequences(&component.render(80).join("\n"));
    assert!(warning.contains("single line"));

    component.handle_input("\n");
    assert_eq!(submitted.borrow().as_slice(), &[""]);

    component.handle_input("synthetic");
    component.handle_input("\x15");
    component.dispose();
    component.handle_input("\x19");
    let after = strip_terminal_sequences(&component.render(80).join("\n"));
    assert!(!after.contains('*'));
}

#[tokio::test]
async fn interactive_mode_isolates_secret_input_from_terminal_listeners_and_settles_on_hide() {
    let (mut mode, _directory, _session) =
        super::runtime35_native::native_mode_with_session_and_provider_retaining_session(
            None,
            |cwd| maho_core::session_manager::SessionManager::in_memory(cwd, None, None),
            "faux",
            "faux-1",
            false,
        );
    let ui = mode.extension_ui.clone();
    assert!(ui.supports_secret_input());

    let seen_by_listener = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let listener_sink = Arc::clone(&seen_by_listener);
    let _unsubscribe = ui
        .on_terminal_input(Arc::new(move |data| {
            listener_sink.lock().expect("lock").push(data.to_owned());
            Some(TerminalInputResult {
                consume: Some(false),
                data: None,
            })
        }))
        .expect("subscribe");

    let pending_fut = ui.input_secret(
        "Private",
        None,
        ExtensionUiDialogOptions {
            secret: true,
            ..Default::default()
        },
    );
    mode.drain_events();

    let sentinel = "secret-key-9876";
    mode.handle_input_at(sentinel, 0);
    let rendered = strip_terminal_sequences(&mode.render(80).join("\n"));
    assert!(!rendered.contains(sentinel));
    assert!(rendered.contains("***************"));
    assert!(seen_by_listener.lock().expect("lock").is_empty());

    mode.handle_input_at("\r", 1);
    mode.drain_events();
    let answer = pending_fut.await.expect("secret result");
    assert_eq!(answer.as_deref(), Some(sentinel));

    mode.handle_input_at("ordinary", 2);
    assert_eq!(
        seen_by_listener.lock().expect("lock").as_slice(),
        &["ordinary".to_owned()]
    );

    let pending_hide_fut = ui.input_secret(
        "Private",
        None,
        ExtensionUiDialogOptions {
            secret: true,
            ..Default::default()
        },
    );
    mode.drain_events();
    mode.handle_input_at("aborted-secret", 3);
    mode.hide_extension_input();
    let hidden_answer = pending_hide_fut.await.expect("secret result");
    assert_eq!(hidden_answer, None);
    let after_hide = strip_terminal_sequences(&mode.render(80).join("\n"));
    assert!(!after_hide.contains("aborted-secret"));
}
