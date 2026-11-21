//! Port of components/extension-input.ts.
//!
//! Single-line text input dialog for extensions. senpi's `setInterval` countdown is host-driven
//! here: call `tick()` once per second.

use std::{cell::RefCell, rc::Rc};

use maho_tui::components::input::{Input, InputOptions};
use maho_tui::components::text::Text;
use maho_tui::keybindings::get_keybindings;
use maho_tui::tui::{Component, Focusable};

use super::keybinding_hints::key_hint;
use super::theme_selector::dynamic_border;
use crate::theme::theme::{Theme, ThemeColor};

#[derive(Default)]
pub struct ExtensionInputOptions {
    /// senpi only arms the countdown when a TUI is present; `false` matches its test doubles.
    pub has_tui: bool,
    pub timeout_ms: Option<u64>,
    /// Text the input opens with; the cursor lands at its end.
    pub initial_value: Option<String>,
    pub secret: bool,
    pub deadline_at_ms: Option<u64>,
}

pub type SubmitHandler = Box<dyn FnMut(&str)>;
pub type CancelHandler = Box<dyn FnMut()>;

pub struct ExtensionInputComponent {
    theme: Theme,
    input: Input,
    base_title: String,
    warning_title: Rc<RefCell<Option<String>>>,
    remaining_seconds: Option<i64>,
    secret_remaining_ms: Option<i64>,
    secret: bool,
    disposed: bool,
    focused: bool,
    on_submit: Rc<RefCell<Option<SubmitHandler>>>,
    on_cancel: Rc<RefCell<Option<CancelHandler>>>,
}

impl ExtensionInputComponent {
    pub fn new(
        theme: &Theme,
        title: &str,
        on_submit: SubmitHandler,
        on_cancel: CancelHandler,
        opts: ExtensionInputOptions,
    ) -> Self {
        let remaining_seconds = if opts.secret {
            None
        } else {
            match (opts.timeout_ms, opts.has_tui) {
                (Some(timeout), true) if timeout > 0 => {
                    Some(i64::try_from(timeout.div_ceil(1000)).unwrap_or(i64::MAX))
                }
                _ => None,
            }
        };
        let secret_remaining_ms = if opts.secret {
            if let Some(deadline) = opts.deadline_at_ms {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as u64);
                Some(i64::try_from(deadline.saturating_sub(now)).unwrap_or(i64::MAX))
            } else {
                opts.timeout_ms
                    .map(|ms| i64::try_from(ms).unwrap_or(i64::MAX))
            }
        } else {
            None
        };
        let warning_title: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let warning_sink = Rc::clone(&warning_title);
        let mut input = Input::new(InputOptions {
            secret: opts.secret,
            on_invalid_input: if opts.secret {
                Some(Rc::new(move || {
                    *warning_sink.borrow_mut() = Some(
                        "Secret input must be a single line without control characters.".to_owned(),
                    );
                }))
            } else {
                None
            },
            ..Default::default()
        });
        let on_submit = Rc::new(RefCell::new(Some(on_submit)));
        let on_cancel = Rc::new(RefCell::new(Some(on_cancel)));
        if opts.secret {
            let submit_cb = Rc::clone(&on_submit);
            input.on_submit = Some(Box::new(move |value| {
                if let Some(handler) = submit_cb.borrow_mut().as_mut() {
                    handler(value);
                }
            }));
            let cancel_cb = Rc::clone(&on_cancel);
            input.on_escape = Some(Box::new(move || {
                if let Some(handler) = cancel_cb.borrow_mut().as_mut() {
                    handler();
                }
            }));
        }
        if let Some(initial_value) = opts.initial_value {
            // Typed in, not assigned: set_value() would leave the cursor at column 0,
            // ahead of the prefill the caller wants the user to edit.
            input.handle_input(&initial_value);
        }
        Self {
            theme: theme.clone(),
            input,
            base_title: title.to_owned(),
            warning_title,
            remaining_seconds,
            secret_remaining_ms,
            secret: opts.secret,
            disposed: false,
            focused: false,
            on_submit,
            on_cancel,
        }
    }

    pub fn is_secret(&self) -> bool {
        self.secret
    }

    pub fn input(&self) -> &Input {
        &self.input
    }

    pub fn input_mut(&mut self) -> &mut Input {
        &mut self.input
    }

    pub fn remaining_seconds(&self) -> Option<i64> {
        self.remaining_seconds
    }

    /// Advance the countdown by one second; expiry cancels the dialog like senpi's interval.
    pub fn tick(&mut self) {
        if self.disposed {
            return;
        }
        if let Some(remaining_ms) = self.secret_remaining_ms.as_mut() {
            *remaining_ms -= 1000;
            if *remaining_ms <= 0 {
                self.disposed = true;
                if let Some(handler) = self.on_cancel.borrow_mut().as_mut() {
                    handler();
                }
            }
            return;
        }
        let Some(remaining) = self.remaining_seconds.as_mut() else {
            return;
        };
        *remaining -= 1;
        if *remaining <= 0 {
            self.disposed = true;
            if let Some(handler) = self.on_cancel.borrow_mut().as_mut() {
                handler();
            }
        }
    }

    pub fn dispose(&mut self) {
        self.disposed = true;
        self.input.dispose();
        *self.on_submit.borrow_mut() = None;
        *self.on_cancel.borrow_mut() = None;
        *self.warning_title.borrow_mut() = None;
    }

    fn title_text(&self) -> String {
        if let Some(warning) = self.warning_title.borrow().as_deref() {
            return self.theme.fg(ThemeColor::Warning, warning);
        }
        let title = match self.remaining_seconds {
            Some(remaining) => format!("{} ({remaining}s)", self.base_title),
            None => self.base_title.clone(),
        };
        self.theme.fg(ThemeColor::Accent, &title)
    }
}

impl Component for ExtensionInputComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = vec![
            dynamic_border(&self.theme, ThemeColor::Border, width),
            String::new(),
        ];
        lines.extend(Text::with_padding(self.title_text(), 1, 0).render(width));
        lines.push(String::new());
        lines.extend(self.input.render(width));
        lines.push(String::new());
        let hints = format!(
            "{}  {}",
            key_hint("tui.select.confirm", "submit", &self.theme),
            key_hint("tui.select.cancel", "cancel", &self.theme)
        );
        lines.extend(Text::with_padding(hints, 1, 0).render(width));
        lines.push(String::new());
        lines.push(dynamic_border(&self.theme, ThemeColor::Border, width));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        if self.disposed {
            return;
        }
        if self.secret {
            self.input.handle_input(data);
            return;
        }
        let kb = get_keybindings();
        if kb.matches(data, "tui.select.confirm") || data == "\n" {
            let value = self.input.get_value().to_owned();
            if let Some(handler) = self.on_submit.borrow_mut().as_mut() {
                handler(&value);
            }
        } else if kb.matches(data, "tui.select.cancel") {
            if let Some(handler) = self.on_cancel.borrow_mut().as_mut() {
                handler();
            }
        } else {
            self.input.handle_input(data);
        }
    }

    fn has_input_handler(&self) -> bool {
        true
    }

    fn dispose(&mut self) {
        self.dispose();
    }

    fn focusable_get(&self) -> Option<bool> {
        Some(self.focused)
    }

    fn focusable_set(&mut self, focused: bool) {
        self.focused = focused;
        self.input.set_focused(focused);
    }
}

impl Focusable for ExtensionInputComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, value: bool) {
        self.focused = value;
        self.input.set_focused(value);
    }
}
