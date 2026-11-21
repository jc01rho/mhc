//! Port of senpi `modes/interactive/attach.ts`: the RPC presentation owns no session runtime,
//! provider, session file, or host process. Endpoint requests run on short-lived threads and report
//! back through one channel that `poll` drains once per frame, so the TUI loop never blocks on the
//! endpoint; only `connect` and `close` block.
use crate::{
    attach_client::{AttachClient, RequestError},
    attach_dialogs::{AttachDialogs, AttachEditorHost},
    chat_viewport::{ChatViewportOptions, create_chat_viewport},
    components::{
        custom_editor::{CustomEditor, CustomEditorOptions},
        extension_editor::editor_theme,
    },
    record_renderer::RecordRenderer,
    theme::theme::{Theme, ThemeColor},
};
use maho_tui::{
    components::{text::Text, v_stack::VStack},
    keybindings::KeybindingsManager,
    tui::{Component, Container, Focusable},
};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    path::Path,
    rc::Rc,
    sync::{Arc, mpsc},
};

/// What an attached TUI declares in `set_client_info`; it renders tool arguments only from
/// `toolcall_end`. Secret input is not declared: this view cannot mask typed text.
pub const ATTACH_CLIENT_CAPABILITIES: [&str; 3] = ["interactive_user", "question", "grant_select"];

pub fn validate_attach_terminal(input_is_tty: bool, output_is_tty: bool) -> Result<(), String> {
    if input_is_tty && output_is_tty {
        Ok(())
    } else {
        Err("--attach requires an interactive terminal on stdin and stdout".to_owned())
    }
}

/// Reports every width its line is drawn at, so the endpoint learns the presentation's width.
struct WidthReporter {
    inner: Text,
    reported: Rc<Cell<Option<usize>>>,
}

impl Component for WidthReporter {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.reported.set(Some(width));
        self.inner.render(width)
    }

    fn invalidate(&mut self) {
        self.inner.invalidate();
    }
}

enum Incoming {
    Record(Value),
    Disconnected(Option<String>),
    ActionFailed {
        message: String,
        restore: Option<String>,
    },
    Answered {
        response_id: String,
        error: Option<RequestError>,
    },
    History {
        entries: Vec<Value>,
        next_before: Option<String>,
    },
}

#[derive(Default)]
struct Link {
    client: Option<Arc<AttachClient>>,
    session_id: Option<String>,
}

pub struct AttachedTui {
    theme: Theme,
    transcript: Rc<RefCell<RecordRenderer>>,
    status: Rc<RefCell<WidthReporter>>,
    dialogs: Rc<RefCell<AttachDialogs>>,
    editor: Rc<RefCell<CustomEditor>>,
    input: Rc<RefCell<Container>>,
    footer: Rc<RefCell<Text>>,
    root: Container,
    link: Rc<RefCell<Link>>,
    incoming: mpsc::Receiver<Incoming>,
    outgoing: mpsc::Sender<Incoming>,
    submissions: Rc<RefCell<VecDeque<String>>>,
    notices: Rc<RefCell<Vec<String>>>,
    finish: Rc<Cell<bool>>,
    abort: Rc<Cell<bool>>,
    reported_width: Rc<Cell<Option<usize>>>,
    answer_deadline_ms: Option<u64>,
    status_value: String,
    state: Option<Value>,
    width: Option<usize>,
    connected: bool,
    attached: bool,
    closing: bool,
    streaming: bool,
    loading_history: bool,
    before: Option<String>,
}

impl AttachedTui {
    pub fn new(
        theme: &Theme,
        keys: Arc<KeybindingsManager>,
        host: Rc<AttachEditorHost>,
        answer_deadline_ms: Option<u64>,
    ) -> Self {
        let (outgoing, incoming) = mpsc::channel();
        let link: Rc<RefCell<Link>> = Rc::default();
        let notices: Rc<RefCell<Vec<String>>> = Rc::default();
        let reported_width: Rc<Cell<Option<usize>>> = Rc::default();
        let transcript = Rc::new(RefCell::new(RecordRenderer::new(theme)));
        let status = Rc::new(RefCell::new(WidthReporter {
            inner: Text::with_padding("Connecting", 1, 0),
            reported: reported_width.clone(),
        }));
        let answer_link = link.clone();
        let answer_tx = outgoing.clone();
        let report_notices = notices.clone();
        let dialogs = Rc::new(RefCell::new(AttachDialogs::new(
            theme,
            keys.clone(),
            host.clone(),
            Box::new(move |response: Value| {
                let response_id = response["id"].as_str().unwrap_or_default().to_owned();
                let (client, session) = {
                    let link = answer_link.borrow();
                    (link.client.clone(), link.session_id.clone())
                };
                let (Some(client), Some(session)) = (client, session) else {
                    let _ = answer_tx.send(Incoming::Answered {
                        response_id,
                        error: Some(RequestError::Failed("Disconnected".to_owned())),
                    });
                    return;
                };
                let tx = answer_tx.clone();
                std::thread::spawn(move || {
                    let error = client.answer(&session, &response).err();
                    let _ = tx.send(Incoming::Answered { response_id, error });
                });
            }),
            Box::new(move |message: String| report_notices.borrow_mut().push(message)),
        )));
        let submissions: Rc<RefCell<VecDeque<String>>> = Rc::default();
        let finish = Rc::new(Cell::new(false));
        let abort = Rc::new(Cell::new(false));
        let mut editor = CustomEditor::new(
            host,
            editor_theme(theme),
            keys,
            CustomEditorOptions::default(),
        );
        let queued = submissions.clone();
        editor.editor.on_submit = Some(Box::new(move |text: &str| {
            queued.borrow_mut().push_back(text.to_owned());
        }));
        let leaving = finish.clone();
        editor.on_ctrl_d = Some(Box::new(move || leaving.set(true)));
        let leaving = finish.clone();
        editor.on_action("app.clear", Box::new(move || leaving.set(true)));
        let aborting = abort.clone();
        editor.on_escape = Some(Box::new(move || aborting.set(true)));
        let editor = Rc::new(RefCell::new(editor));
        let input = Rc::new(RefCell::new(Container::new()));
        input.borrow_mut().add_child(dialogs.clone());
        input.borrow_mut().add_child(editor.clone());
        let footer = Rc::new(RefCell::new(Text::with_padding(
            "/history  /steer <text>  /abort  /quit",
            1,
            0,
        )));
        let mut root = Container::new();
        root.add_child(transcript.clone());
        root.add_child(status.clone());
        root.add_child(input.clone());
        root.add_child(footer.clone());
        Self {
            theme: theme.clone(),
            transcript,
            status,
            dialogs,
            editor,
            input,
            footer,
            root,
            link,
            incoming,
            outgoing,
            submissions,
            notices,
            finish,
            abort,
            reported_width,
            answer_deadline_ms,
            status_value: "Connecting".to_owned(),
            state: None,
            width: None,
            connected: false,
            attached: false,
            closing: false,
            streaming: false,
            loading_history: false,
            before: None,
        }
    }

    /// The root of the alternate screen: the transcript scrolls above the pinned dock.
    pub fn layout_root(&self) -> Rc<RefCell<VStack>> {
        let empty: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(Container::new()));
        create_chat_viewport(ChatViewportOptions {
            document: self.transcript.clone(),
            pending_messages: empty,
            status: self.status.clone(),
            hook_status: None,
            editor: self.input.clone(),
            footer: self.footer.clone(),
            widgets_above: None,
            widgets_below: None,
            scrollbar: None,
            scrollbar_track_style: None,
            scrollbar_thumb_style: None,
        })
        .root
    }

    /// True once the user asked to leave (`/quit`, Ctrl+D, or the clear action).
    pub fn finished(&self) -> bool {
        self.finish.get()
    }

    pub fn status(&self) -> &str {
        &self.status_value
    }

    pub fn connect(
        &mut self,
        address: &str,
        session_ref: &str,
        columns: usize,
        key_root: Option<&Path>,
    ) -> Result<(), String> {
        let path = address.strip_prefix("unix://").unwrap_or(address);
        if path.is_empty()
            || path.contains("://")
            || path.to_ascii_lowercase().starts_with("\\\\.\\pipe\\")
        {
            return Err(
                "--attach needs a local socket path or a named pipe's logical filesystem path"
                    .to_owned(),
            );
        }
        let attached = self.attach(path, session_ref, columns, key_root);
        if attached.is_err() {
            let _ = self.close();
        }
        attached
    }

    fn attach(
        &mut self,
        path: &str,
        session_ref: &str,
        columns: usize,
        key_root: Option<&Path>,
    ) -> Result<(), String> {
        let records = self.outgoing.clone();
        let disconnects = self.outgoing.clone();
        let client = Arc::new(AttachClient::connect(
            path,
            key_root,
            Box::new(move |record| {
                let _ = records.send(Incoming::Record(record));
            }),
            Box::new(move |reason| {
                let _ = disconnects.send(Incoming::Disconnected(reason));
            }),
            self.answer_deadline_ms,
        )?);
        self.link.borrow_mut().client = Some(client.clone());
        self.connected = true;
        self.width = Some(columns);
        let ask = |command: Value, expected: &str| -> Result<Value, String> {
            let response = client.request(command).map_err(|error| error.to_string())?;
            if response["command"] == expected {
                Ok(response)
            } else {
                Err(format!("Expected {expected} from the endpoint"))
            }
        };
        client
            .request(json!({
                "type": "set_client_info",
                "width": columns,
                "capabilities": ATTACH_CLIENT_CAPABILITIES,
            }))
            .map_err(|error| error.to_string())?;
        let listing = ask(
            json!({"type": "list_sessions", "observe": true}),
            "list_sessions",
        )?;
        let rows: Vec<&Value> = listing["data"]["sessions"]
            .as_array()
            .map(|rows| rows.iter().collect())
            .unwrap_or_default();
        let exact: Vec<&Value> = rows
            .iter()
            .copied()
            .filter(|row| {
                ["sessionId", "durableSessionId", "sessionPath"]
                    .iter()
                    .any(|field| row[*field].as_str() == Some(session_ref))
            })
            .collect();
        let matches: Vec<&Value> = if exact.is_empty() {
            rows.iter()
                .copied()
                .filter(|row| {
                    row["durableSessionId"]
                        .as_str()
                        .is_some_and(|id| id.starts_with(session_ref))
                })
                .collect()
        } else {
            exact
        };
        let row = match matches.as_slice() {
            [row] if row["durableSessionId"].is_string() => *row,
            [_, _, ..] => return Err("Ambiguous session reference".to_owned()),
            _ => return Err("No live session matches the reference".to_owned()),
        };
        if row["retain_on_disconnect"] != true {
            return Err(format!(
                "Session {session_ref} is not retained on disconnect: the endpoint ends it when its last client leaves, so leaving this view could end it. Attach to a session opened with retain_on_disconnect."
            ));
        }
        let session_id = row["sessionId"].as_str().unwrap_or_default().to_owned();
        let durable = row["durableSessionId"].clone();
        self.link.borrow_mut().session_id = Some(session_id.clone());
        ask(
            json!({"type": "attach_session", "sessionId": session_id, "durableSessionId": durable}),
            "attach_session",
        )?;
        self.attached = true;
        let subscribed = ask(
            json!({"type": "subscribe_sessions", "sessions": {"ids": [session_id]}, "mirror": "all"}),
            "subscribe_sessions",
        )?;
        let state = ask(
            json!({"type": "get_state", "sessionId": session_id, "observe": true}),
            "get_state",
        )?;
        self.streaming = state["data"]["isStreaming"] == true;
        self.state = Some(state["data"].clone());
        self.read_history(&client)?;
        let pending: Vec<Value> = ["pendingDialogs", "pendingQuestions"]
            .iter()
            .filter_map(|field| state["data"][*field].as_array())
            .flatten()
            .cloned()
            .collect();
        for dialog in &pending {
            self.dialogs.borrow_mut().accept(dialog);
        }
        self.show_status(if self.streaming {
            "Running"
        } else {
            "Attached"
        });
        for status in subscribed["data"]["snapshot"]
            .as_array()
            .map_or(&[][..], Vec::as_slice)
        {
            self.record(status);
        }
        Ok(())
    }

    fn history_command(&self, session_id: &str) -> Value {
        let mut command = json!({
            "type": "get_entries", "sessionId": session_id, "tail": 100, "observe": true,
        });
        if let Some(before) = &self.before {
            command["before"] = Value::String(before.clone());
        }
        command
    }

    fn read_history(&mut self, client: &AttachClient) -> Result<(), String> {
        let Some(session_id) = self.link.borrow().session_id.clone() else {
            return Ok(());
        };
        let response = client
            .request(self.history_command(&session_id))
            .map_err(|error| error.to_string())?;
        if response["command"] != "get_entries" {
            return Err("Expected history entries".to_owned());
        }
        self.before = response["data"]["next_before"].as_str().map(str::to_owned);
        let entries = response["data"]["entries"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        self.transcript.borrow_mut().prepend(&entries);
        Ok(())
    }

    /// Drains what arrived since the last frame and runs queued submissions. Call once per frame.
    pub fn poll(&mut self) {
        while let Ok(item) = self.incoming.try_recv() {
            self.handle_incoming(item);
        }
        let notices = std::mem::take(&mut *self.notices.borrow_mut());
        for notice in notices {
            self.show_status(&notice);
        }
        let submissions: Vec<String> = self.submissions.borrow_mut().drain(..).collect();
        for text in submissions {
            self.submit(text);
        }
        if self.abort.replace(false) {
            self.abort_turn();
        }
        self.report_width();
    }

    fn handle_incoming(&mut self, item: Incoming) {
        match item {
            Incoming::Record(record) => {
                let session = self.link.borrow().session_id.clone();
                if record["sessionId"]
                    .as_str()
                    .is_some_and(|id| Some(id) != session.as_deref())
                {
                    return;
                }
                self.record(&record);
            }
            Incoming::Disconnected(reason) => {
                self.connected = false;
                self.dialogs.borrow_mut().dispose();
                self.show_status(&match reason {
                    Some(reason) => format!("Disconnected: {reason}"),
                    None => "Disconnected".to_owned(),
                });
            }
            Incoming::ActionFailed { message, restore } => {
                self.loading_history = false;
                if let Some(text) = restore {
                    self.restore_text(&text);
                }
                self.show_status(&message);
            }
            Incoming::Answered { response_id, error } => match error {
                None => self.dialogs.borrow_mut().answer_accepted(&response_id),
                Some(error) => self
                    .dialogs
                    .borrow_mut()
                    .answer_failed(&response_id, &error),
            },
            Incoming::History {
                entries,
                next_before,
            } => {
                self.loading_history = false;
                self.before = next_before;
                self.transcript.borrow_mut().prepend(&entries);
            }
        }
    }

    fn record(&mut self, record: &Value) {
        self.transcript.borrow_mut().accept(record);
        match record["type"].as_str() {
            Some("extension_ui_request") => {
                if record["method"] == "notify" {
                    self.show_status(record["message"].as_str().unwrap_or_default());
                } else {
                    self.dialogs.borrow_mut().accept(record);
                }
            }
            Some("question_resolved" | "secret_input_settled") => {
                if let Some(id) = record["id"].as_str() {
                    self.dialogs.borrow_mut().resolve(id);
                }
            }
            Some("session_status") => {
                self.streaming = record["status"] == "running";
                self.show_status(record["status"].as_str().unwrap_or_default());
            }
            Some("agent_start") => {
                self.streaming = true;
                self.show_status("Running");
            }
            Some("agent_end" | "agent_settled") => {
                self.streaming = false;
                self.show_status("Attached");
            }
            Some("model_changed") => {
                if let Some(state) = &mut self.state {
                    state["model"] = record["model"].clone();
                    state["thinkingLevel"] = record["thinkingLevel"].clone();
                }
                self.show_status(if self.streaming {
                    "Running"
                } else {
                    "Attached"
                });
            }
            _ => {}
        }
    }

    fn show_status(&mut self, value: &str) {
        let state = self.state.as_ref();
        let name = state
            .and_then(|state| state["sessionName"].as_str())
            .map(|name| format!(" - {name}"))
            .unwrap_or_default();
        let model = state
            .filter(|state| state["model"].is_object())
            .map(|state| {
                format!(
                    " - {}/{}",
                    state["model"]["provider"].as_str().unwrap_or_default(),
                    state["model"]["id"].as_str().unwrap_or_default()
                )
            })
            .unwrap_or_default();
        self.status_value = format!("{value}{name}{model}");
        let line = self.theme.fg(ThemeColor::Muted, &self.status_value);
        self.status.borrow_mut().inner.set_text(line);
    }

    fn report_width(&mut self) {
        if !self.attached || !self.connected {
            return;
        }
        let Some(width) = self.reported_width.get() else {
            return;
        };
        if Some(width) == self.width {
            return;
        }
        self.width = Some(width);
        self.perform(
            move |client| {
                client
                    .request(json!({"type": "set_client_info", "width": width}))
                    .map(|_| ())
            },
            None,
        );
    }

    fn perform(
        &self,
        action: impl FnOnce(&AttachClient) -> Result<(), RequestError> + Send + 'static,
        restore: Option<String>,
    ) {
        let client = self.link.borrow().client.clone();
        let Some(client) = client else {
            let _ = self.outgoing.send(Incoming::ActionFailed {
                message: "Disconnected".to_owned(),
                restore,
            });
            return;
        };
        let tx = self.outgoing.clone();
        std::thread::spawn(move || {
            if let Err(error) = action(&client) {
                let _ = tx.send(Incoming::ActionFailed {
                    message: error.to_string(),
                    restore,
                });
            }
        });
    }

    fn submit(&mut self, text: String) {
        let trimmed = text.trim();
        if trimmed == "/quit" {
            self.finish.set(true);
            return;
        }
        let session_id = self.link.borrow().session_id.clone();
        let Some(session_id) = session_id.filter(|_| self.connected) else {
            self.show_status("Disconnected; attach again to send input");
            self.restore_text(&text);
            return;
        };
        if trimmed == "/history" {
            if self.before.is_some() && !self.loading_history {
                self.load_history(&session_id);
            }
            return;
        }
        if trimmed == "/abort" {
            self.abort_turn();
            return;
        }
        let steering = text.starts_with("/steer ");
        let message = text.strip_prefix("/steer ").unwrap_or(&text).to_owned();
        if message.trim().is_empty() {
            return;
        }
        let kind = if steering || self.streaming {
            "steer"
        } else {
            "prompt"
        };
        let command = json!({
            "type": kind,
            "sessionId": session_id,
            "message": message,
            "clientMessageId": uuid::Uuid::new_v4().to_string(),
        });
        self.perform(
            move |client| client.request(command).map(|_| ()),
            Some(text),
        );
    }

    fn restore_text(&mut self, text: &str) {
        if self.editor.borrow().editor.get_text().is_empty() {
            self.editor.borrow_mut().editor.set_text(text);
        }
    }

    fn load_history(&mut self, session_id: &str) {
        let Some(client) = self.link.borrow().client.clone() else {
            return;
        };
        self.loading_history = true;
        let command = self.history_command(session_id);
        let tx = self.outgoing.clone();
        std::thread::spawn(move || {
            let outcome = match client.request(command) {
                Ok(response) if response["command"] == "get_entries" => Incoming::History {
                    entries: response["data"]["entries"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default(),
                    next_before: response["data"]["next_before"].as_str().map(str::to_owned),
                },
                Ok(_) => Incoming::ActionFailed {
                    message: "Expected history entries".to_owned(),
                    restore: None,
                },
                Err(error) => Incoming::ActionFailed {
                    message: error.to_string(),
                    restore: None,
                },
            };
            let _ = tx.send(outcome);
        });
    }

    fn abort_turn(&self) {
        let session_id = self.link.borrow().session_id.clone();
        let Some(session_id) = session_id.filter(|_| self.connected) else {
            return;
        };
        let command = json!({
            "type": "abort", "sessionId": session_id, "intent": "stop",
            "effectId": uuid::Uuid::new_v4().to_string(),
        });
        self.perform(move |client| client.request(command).map(|_| ()), None);
    }

    /// Releases the attachment (so the endpoint may park the session) and closes the socket. The
    /// release waits for the endpoint, so the terminal is restored before this is called.
    pub fn close(&mut self) -> Result<(), String> {
        if self.closing {
            return Ok(());
        }
        self.closing = true;
        self.dialogs.borrow_mut().dispose();
        let (client, session_id) = {
            let link = self.link.borrow();
            (link.client.clone(), link.session_id.clone())
        };
        let mut released = Ok(());
        if let (Some(client), Some(session_id)) = (&client, &session_id)
            && self.connected
            && self.attached
        {
            released = client
                .request(json!({"type": "release_attachment", "sessionId": session_id}))
                .map(|_| ())
                .map_err(|error| error.to_string());
        }
        self.connected = false;
        if let Some(client) = client {
            client.close();
        }
        released
    }
}

impl Component for AttachedTui {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.root.render(width)
    }

    fn handle_input(&mut self, data: &str) {
        if self.dialogs.borrow().active() {
            self.dialogs.borrow_mut().handle_input(data);
        } else {
            self.editor.borrow_mut().handle_input(data);
        }
    }

    fn has_input_handler(&self) -> bool {
        true
    }

    fn invalidate(&mut self) {
        self.root.invalidate();
    }

    fn focusable_get(&self) -> Option<bool> {
        Some(self.editor.borrow().focused())
    }

    fn focusable_set(&mut self, focused: bool) {
        self.editor.borrow_mut().set_focused(focused);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{
        attach_test_endpoint::{AnswerBehavior, Endpoint, EndpointOptions},
        theme::ColorMode,
    };
    use std::time::{Duration, Instant};

    fn new_view() -> AttachedTui {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().to_string_lossy().into_owned();
        let keys = Arc::new(
            maho_core::keybindings::KeybindingsManager::create(Some(&agent_dir))
                .inner()
                .clone(),
        );
        AttachedTui::new(
            &Theme::builtin("dark", ColorMode::Truecolor).unwrap(),
            keys,
            Rc::new(AttachEditorHost::new(30)),
            Some(200),
        )
    }

    fn attached(options: EndpointOptions) -> (Endpoint, AttachedTui) {
        let endpoint = Endpoint::start(options);
        let mut view = new_view();
        view.connect(&endpoint.path, "durable", 80, None).unwrap();
        (endpoint, view)
    }

    fn wait_until(view: &mut AttachedTui, done: impl Fn(&AttachedTui) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        view.poll();
        while !done(view) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "timed out; status {:?}",
                view.status()
            );
            let item = view.incoming.recv_timeout(remaining).unwrap();
            view.handle_incoming(item);
            view.poll();
        }
    }

    #[test]
    fn connect_attaches_in_order_loads_history_and_declares_its_capabilities() {
        let (endpoint, view) = attached(EndpointOptions::default());
        let kinds: Vec<String> = endpoint
            .commands()
            .iter()
            .map(|command| command["type"].as_str().unwrap_or_default().to_owned())
            .collect();
        assert_eq!(
            kinds,
            [
                "set_client_info",
                "list_sessions",
                "attach_session",
                "subscribe_sessions",
                "get_state",
                "get_entries"
            ]
        );
        let info = endpoint.command("set_client_info");
        assert_eq!(info["width"], 80);
        let capabilities = info["capabilities"].as_array().unwrap();
        assert!(capabilities.contains(&json!("interactive_user")));
        assert!(!capabilities.contains(&json!("secret_input_v1")));
        assert_eq!(
            endpoint.command("attach_session")["durableSessionId"],
            "durable"
        );
        assert_eq!(endpoint.command("subscribe_sessions")["mirror"], "all");
        assert_eq!(view.transcript.borrow().row_count(), 1);
        assert_eq!(view.status(), "Attached");
        assert!(!view.finished());
    }

    #[test]
    fn a_session_the_endpoint_would_end_on_disconnect_is_refused_before_attaching() {
        let endpoint = Endpoint::start(EndpointOptions {
            retained: Some(false),
            ..Default::default()
        });
        let mut view = new_view();
        let error = view
            .connect(&endpoint.path, "durable", 80, None)
            .unwrap_err();
        assert!(error.contains("is not retained on disconnect"), "{error}");
        assert!(
            endpoint
                .commands()
                .iter()
                .all(|command| command["type"] != "attach_session")
        );
    }

    #[test]
    fn a_reference_matches_by_prefix_and_an_unknown_one_names_the_failure() {
        let endpoint = Endpoint::start(EndpointOptions::default());
        let mut view = new_view();
        view.connect(&endpoint.path, "dur", 80, None).unwrap();
        let other = Endpoint::start(EndpointOptions::default());
        let mut missing = new_view();
        let error = missing
            .connect(&other.path, "nothing", 80, None)
            .unwrap_err();
        assert_eq!(error, "No live session matches the reference");
    }

    #[test]
    fn an_address_that_is_not_a_local_socket_path_is_refused() {
        let mut view = new_view();
        for address in ["", "tcp://host:1", "\\\\.\\pipe\\x"] {
            assert_eq!(
                view.connect(address, "durable", 80, None).unwrap_err(),
                "--attach needs a local socket path or a named pipe's logical filesystem path"
            );
        }
    }

    #[test]
    fn input_goes_out_as_a_prompt_then_as_a_steer_while_a_turn_runs() {
        let (endpoint, mut view) = attached(EndpointOptions::default());
        view.submit("hello".to_owned());
        let prompt = endpoint.command("prompt");
        assert_eq!(prompt["message"], "hello");
        assert_eq!(prompt["sessionId"], "live");
        assert!(prompt["clientMessageId"].as_str().is_some());
        endpoint.send(&json!({"type": "agent_start"}));
        wait_until(&mut view, |view| view.status() == "Running");
        view.submit("/steer later".to_owned());
        assert_eq!(endpoint.command("steer")["message"], "later");
        view.submit("keep going".to_owned());
        endpoint
            .wait_for(|command| command["type"] == "steer" && command["message"] == "keep going");
    }

    #[test]
    fn a_refused_input_comes_back_into_the_editor_with_the_reason() {
        let mut options = EndpointOptions::default();
        options
            .refuse
            .insert("prompt".to_owned(), "busy".to_owned());
        let (_endpoint, mut view) = attached(options);
        view.submit("hello".to_owned());
        wait_until(&mut view, |view| view.status() == "busy");
        assert_eq!(view.editor.borrow().editor.get_text(), "hello");
    }

    #[test]
    fn a_dialog_the_endpoint_raises_is_answered_and_a_refusal_is_reported() {
        let (endpoint, mut view) = attached(EndpointOptions {
            answer: AnswerBehavior::Refuse("question_already_resolved".to_owned()),
            ..Default::default()
        });
        endpoint.send(&json!({
            "type": "extension_ui_request", "method": "select", "id": "d1",
            "title": "Pick", "options": ["a", "b"],
        }));
        wait_until(&mut view, |view| view.dialogs.borrow().active());
        view.handle_input("\r");
        let answer = endpoint.command("extension_ui_response");
        assert_eq!(answer["id"], "d1");
        assert_eq!(answer["value"], "a");
        assert_eq!(answer["sessionId"], "live");
        wait_until(&mut view, |view| view.status().starts_with("Not accepted"));
        assert!(!view.dialogs.borrow().active());
    }

    #[test]
    fn a_record_of_another_session_is_ignored_and_a_resolved_dialog_leaves() {
        let (endpoint, mut view) = attached(EndpointOptions::default());
        endpoint.send(&json!({
            "type": "extension_ui_request", "method": "select", "id": "d1",
            "title": "Pick", "options": ["a"],
        }));
        wait_until(&mut view, |view| view.dialogs.borrow().active());
        endpoint.send_raw(r#"{"type":"agent_end","sessionId":"elsewhere"}"#);
        endpoint.send(&json!({"type": "question_resolved", "id": "d1"}));
        wait_until(&mut view, |view| !view.dialogs.borrow().active());
        assert_eq!(view.status(), "Attached");
    }

    #[test]
    fn the_width_the_view_is_drawn_at_is_reported_to_the_endpoint() {
        let (endpoint, mut view) = attached(EndpointOptions::default());
        view.render(100);
        view.poll();
        endpoint
            .wait_for(|command| command["type"] == "set_client_info" && command["width"] == 100);
    }

    #[test]
    fn close_releases_the_attachment_once_and_a_lost_connection_is_shown() {
        let (endpoint, mut view) = attached(EndpointOptions::default());
        assert_eq!(view.close(), Ok(()));
        assert_eq!(view.close(), Ok(()));
        let releases = endpoint
            .commands()
            .iter()
            .filter(|command| command["type"] == "release_attachment")
            .count();
        assert_eq!(releases, 1);
        assert_eq!(endpoint.command("release_attachment")["sessionId"], "live");

        let (endpoint, mut view) = attached(EndpointOptions::default());
        endpoint.disconnect();
        wait_until(&mut view, |view| view.status().starts_with("Disconnected"));
        view.submit("hello".to_owned());
        assert_eq!(view.status(), "Disconnected; attach again to send input");
        assert_eq!(view.editor.borrow().editor.get_text(), "hello");
    }

    #[test]
    fn quit_ends_the_view_and_validation_demands_a_terminal() {
        let (_endpoint, mut view) = attached(EndpointOptions::default());
        view.submit("/quit".to_owned());
        assert!(view.finished());
        assert!(validate_attach_terminal(true, true).is_ok());
        assert!(validate_attach_terminal(true, false).is_err());
    }
}
