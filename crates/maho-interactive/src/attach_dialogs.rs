//! Port of senpi `modes/interactive/attach-dialogs.ts`: owns only the visible dialog; the endpoint
//! owns its resolution and lifetime. A component's answer lands in an outbox and is sent after the
//! component's input call returns; the outcome of the send comes back through `answer_accepted` or
//! `answer_failed`, because the TUI loop never blocks on the endpoint.
use crate::{
    attach_client::RequestError,
    components::{
        ask_user_question::{AskUserQuestionComponent, AskUserQuestionOptions},
        extension_editor::ExtensionEditorComponent,
        extension_input::{ExtensionInputComponent, ExtensionInputOptions},
        extension_selector::{ExtensionSelectorComponent, ExtensionSelectorOptions},
    },
    theme::theme::{Theme, ThemeColor},
};
use maho_ext_api::{Question, QuestionOption, QuestionRequest, QuestionResponse, QuestionStatus};
use maho_tui::{
    components::{
        editor::{EditorOptions, EditorTuiHost},
        text::Text,
    },
    keybindings::KeybindingsManager,
    tui::{Component, Container},
};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet, VecDeque},
    rc::Rc,
    sync::Arc,
};

/// The editor host of the attached view: repaints are driven by the loop, the row count is the terminal's.
pub struct AttachEditorHost {
    rows: Cell<usize>,
}

impl AttachEditorHost {
    pub fn new(rows: usize) -> Self {
        Self {
            rows: Cell::new(rows),
        }
    }

    pub fn set_rows(&self, rows: usize) {
        self.rows.set(rows);
    }
}

impl EditorTuiHost for AttachEditorHost {
    fn request_render(&self) {}

    fn terminal_rows(&self) -> usize {
        self.rows.get()
    }

    fn full_redraws(&self) -> u64 {
        0
    }
}

/// "Requested from <device>" for a dialog raised by a remote device's turn (senpi#5480), read from the
/// frame's `remoteOrigin`. Absent or malformed means a local turn: no label. The name is wire data, so
/// control characters are dropped before it reaches the terminal.
fn remote_origin_label(request: &Value) -> Option<String> {
    let origin = request.get("remoteOrigin")?;
    origin.get("deviceId")?;
    let clean = |value: &Value| {
        value
            .as_str()
            .map(|text| text.chars().filter(|c| !c.is_control()).collect::<String>())
            .map(|text| text.trim().to_owned())
            .unwrap_or_default()
    };
    let name = clean(&origin["deviceName"]);
    let id = clean(&origin["deviceId"]);
    let device = if name.is_empty() { id } else { name };
    (!device.is_empty()).then(|| format!("Requested from {device}"))
}

type Respond = Rc<dyn Fn(Value)>;

fn value_reply(respond: &Respond, id: &str) -> Box<dyn FnMut(&str)> {
    let respond = respond.clone();
    let id = id.to_owned();
    Box::new(move |value: &str| {
        respond(json!({"type": "extension_ui_response", "id": id, "value": value}));
    })
}

fn confirm_reply(respond: &Respond, id: &str) -> Box<dyn FnMut(&str)> {
    let respond = respond.clone();
    let id = id.to_owned();
    Box::new(move |value: &str| {
        respond(json!({"type": "extension_ui_response", "id": id, "confirmed": value == "Yes"}));
    })
}

fn cancel_reply(respond: &Respond, id: &str) -> Box<dyn FnMut()> {
    let respond = respond.clone();
    let id = id.to_owned();
    Box::new(move || {
        respond(json!({"type": "extension_ui_response", "id": id, "cancelled": true}));
    })
}

fn question_request(request: &Value) -> QuestionRequest {
    let questions = request["questions"]
        .as_array()
        .map(|questions| {
            questions
                .iter()
                .map(|question| Question {
                    id: question["id"].as_str().unwrap_or_default().to_owned(),
                    header: question["header"].as_str().unwrap_or_default().to_owned(),
                    question: question["question"].as_str().unwrap_or_default().to_owned(),
                    options: question["options"]
                        .as_array()
                        .map(|options| {
                            options
                                .iter()
                                .map(|option| QuestionOption {
                                    label: option["label"].as_str().unwrap_or_default().to_owned(),
                                    description: option["description"].as_str().map(str::to_owned),
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    multi_select: question["multiSelect"].as_bool().unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_default();
    QuestionRequest {
        request_id: request["requestId"].as_str().unwrap_or_default().to_owned(),
        questions,
        wait_for_answer: request["waitForAnswer"].as_bool().unwrap_or(false),
        timeout_ms: 0,
    }
}

fn question_reply(id: &str, response: &QuestionResponse) -> Value {
    if !matches!(
        response.status,
        QuestionStatus::Answered | QuestionStatus::CommentSubmitted
    ) {
        return json!({"type": "extension_ui_response", "id": id, "cancelled": true});
    }
    let answers: serde_json::Map<String, Value> = response
        .answers
        .iter()
        .map(|(question, answer)| {
            (
                question.clone(),
                json!({"selected": answer.selected, "text": answer.text}),
            )
        })
        .collect();
    let mut reply = json!({"type": "extension_ui_response", "id": id, "answers": answers});
    if let Some(comment) = &response.comment {
        reply["comment"] = Value::String(comment.clone());
    }
    reply
}

struct Current {
    request: Value,
    component: Rc<RefCell<dyn Component>>,
}

#[derive(Default)]
struct DialogState {
    queue: VecDeque<Value>,
    seen: HashSet<String>,
    settled: HashSet<String>,
    current: Option<Current>,
    closed: bool,
    in_flight: HashMap<String, Value>,
    outbox: Vec<(String, Value)>,
}

pub struct AttachDialogs {
    state: Rc<RefCell<DialogState>>,
    container: Container,
    theme: Theme,
    keys: Arc<KeybindingsManager>,
    host: Rc<dyn EditorTuiHost>,
    answer: Box<dyn FnMut(Value)>,
    report: Box<dyn FnMut(String)>,
}

impl AttachDialogs {
    /// `answer` sends one response to the endpoint without waiting; `report` shows a status line.
    pub fn new(
        theme: &Theme,
        keys: Arc<KeybindingsManager>,
        host: Rc<dyn EditorTuiHost>,
        answer: Box<dyn FnMut(Value)>,
        report: Box<dyn FnMut(String)>,
    ) -> Self {
        Self {
            state: Rc::default(),
            container: Container::new(),
            theme: theme.clone(),
            keys,
            host,
            answer,
            report,
        }
    }

    pub fn active(&self) -> bool {
        self.state.borrow().current.is_some()
    }

    pub fn accept(&mut self, request: &Value) {
        let Some(id) = request["id"].as_str() else {
            return;
        };
        if !matches!(
            request["method"].as_str(),
            Some("select" | "confirm" | "input" | "editor" | "question")
        ) {
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            if !state.seen.insert(id.to_owned()) {
                return;
            }
            state.queue.push_back(request.clone());
        }
        self.show_next();
    }

    /// The endpoint settled the request (another client answered it): it never comes back.
    pub fn resolve(&mut self, id: &str) {
        let is_current = {
            let mut state = self.state.borrow_mut();
            state.settled.insert(id.to_owned());
            state
                .queue
                .retain(|request| request["id"].as_str() != Some(id));
            state
                .current
                .as_ref()
                .is_some_and(|current| current.request["id"].as_str() == Some(id))
        };
        if is_current {
            self.dismiss();
        }
    }

    pub fn handle_input(&mut self, data: &str) {
        let component = self
            .state
            .borrow()
            .current
            .as_ref()
            .map(|current| current.component.clone());
        if let Some(component) = component {
            component.borrow_mut().handle_input(data);
        }
        self.flush();
    }

    pub fn dispose(&mut self) {
        let current = {
            let mut state = self.state.borrow_mut();
            state.closed = true;
            state.queue.clear();
            state.outbox.clear();
            state.current.take()
        };
        if let Some(current) = current {
            current.component.borrow_mut().dispose();
        }
        self.container.detach_all();
    }

    /// The endpoint accepted the answer carried by the response `id`.
    pub fn answer_accepted(&mut self, response_id: &str) {
        self.state.borrow_mut().in_flight.remove(response_id);
    }

    /// The dialog left while its answer was in flight and comes back unless the endpoint accepted it.
    /// A request the endpoint no longer waits on (settled elsewhere, or gone) stays dismissed.
    pub fn answer_failed(&mut self, response_id: &str, error: &RequestError) {
        let Some(request) = self.state.borrow_mut().in_flight.remove(response_id) else {
            return;
        };
        if let RequestError::AnswerRefused(code) = error
            && matches!(
                code.as_str(),
                "question_already_resolved" | "unknown_extension_ui_request"
            )
        {
            (self.report)(format!("Not accepted: {code} (already settled)"));
            return;
        }
        let id = request["id"].as_str().unwrap_or_default().to_owned();
        {
            let state = self.state.borrow();
            if state.closed || state.settled.contains(&id) {
                return;
            }
        }
        (self.report)(format!("{error}. Answer again."));
        self.state.borrow_mut().queue.push_front(request);
        self.show_next();
    }

    fn responder(&self, request_id: &str) -> Respond {
        let state = self.state.clone();
        let request_id = request_id.to_owned();
        Rc::new(move |response: Value| {
            state
                .borrow_mut()
                .outbox
                .push((request_id.clone(), response));
        })
    }

    fn flush(&mut self) {
        let outbox = std::mem::take(&mut self.state.borrow_mut().outbox);
        for (request_id, response) in outbox {
            let request = {
                let state = self.state.borrow();
                state
                    .current
                    .as_ref()
                    .filter(|current| current.request["id"].as_str() == Some(request_id.as_str()))
                    .map(|current| current.request.clone())
            };
            if let Some(request) = request {
                self.submit(request, &response);
            }
        }
    }

    fn submit(&mut self, request: Value, response: &Value) {
        self.dismiss();
        let response_id = response["id"].as_str().unwrap_or_default().to_owned();
        self.state
            .borrow_mut()
            .in_flight
            .insert(response_id, request);
        (self.answer)(response.clone());
    }

    fn dismiss(&mut self) {
        let current = self.state.borrow_mut().current.take();
        if let Some(current) = current {
            current.component.borrow_mut().dispose();
        }
        self.container.detach_all();
        self.show_next();
    }

    fn show_next(&mut self) {
        if self.state.borrow().current.is_some() {
            return;
        }
        let Some(request) = self.state.borrow_mut().queue.pop_front() else {
            return;
        };
        let id = request["id"].as_str().unwrap_or_default().to_owned();
        let title = request["title"].as_str().unwrap_or_default().to_owned();
        let respond = self.responder(&id);
        let component: Rc<RefCell<dyn Component>> = match request["method"].as_str() {
            Some("select") => {
                let options = request["options"]
                    .as_array()
                    .map(|options| {
                        options
                            .iter()
                            .filter_map(|option| option.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                Rc::new(RefCell::new(self.selector(
                    &title,
                    options,
                    value_reply(&respond, &id),
                    cancel_reply(&respond, &id),
                )))
            }
            Some("confirm") => {
                let message = request["message"].as_str().unwrap_or_default();
                let title = if message.is_empty() {
                    title
                } else {
                    format!("{title} - {message}")
                };
                Rc::new(RefCell::new(self.selector(
                    &title,
                    vec!["Yes".to_owned(), "No".to_owned()],
                    confirm_reply(&respond, &id),
                    cancel_reply(&respond, &id),
                )))
            }
            Some("input") => {
                if request["secret"] == true {
                    // This view cannot mask typed text, so it never declares secret input.
                    self.state.borrow_mut().settled.insert(id);
                    (self.report)(
                        "A secret input request was ignored: it cannot be answered here".to_owned(),
                    );
                    self.show_next();
                    return;
                }
                Rc::new(RefCell::new(ExtensionInputComponent::new(
                    &self.theme,
                    &title,
                    value_reply(&respond, &id),
                    cancel_reply(&respond, &id),
                    ExtensionInputOptions::default(),
                )))
            }
            Some("editor") => Rc::new(RefCell::new(ExtensionEditorComponent::new(
                &self.theme,
                self.host.clone(),
                self.keys.clone(),
                &title,
                request["prefill"].as_str(),
                value_reply(&respond, &id),
                cancel_reply(&respond, &id),
                EditorOptions::default(),
                None,
                None,
            ))),
            Some("question") => {
                let answered = respond.clone();
                let answered_id = id.clone();
                Rc::new(RefCell::new(AskUserQuestionComponent::new(
                    question_request(&request),
                    Box::new(move |response: QuestionResponse| {
                        answered(question_reply(&answered_id, &response));
                    }),
                    AskUserQuestionOptions::new(self.theme.clone()),
                )))
            }
            _ => return,
        };
        component.borrow_mut().focusable_set(true);
        if let Some(origin) = remote_origin_label(&request) {
            self.container
                .add_child(Rc::new(RefCell::new(Text::with_padding(
                    self.theme.fg(ThemeColor::Warning, &origin),
                    1,
                    0,
                ))));
        }
        self.container.add_child(component.clone());
        self.state.borrow_mut().current = Some(Current { request, component });
    }

    fn selector(
        &self,
        title: &str,
        options: Vec<String>,
        on_select: Box<dyn FnMut(&str)>,
        on_cancel: Box<dyn FnMut()>,
    ) -> ExtensionSelectorComponent {
        ExtensionSelectorComponent::new(
            &self.theme,
            self.keys.clone(),
            title,
            options,
            on_select,
            on_cancel,
            ExtensionSelectorOptions {
                terminal_rows: Some(self.host.terminal_rows()),
                ..Default::default()
            },
        )
    }
}

impl Component for AttachDialogs {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    fn handle_input(&mut self, data: &str) {
        AttachDialogs::handle_input(self, data);
    }

    fn has_input_handler(&self) -> bool {
        true
    }

    fn invalidate(&mut self) {
        self.container.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ColorMode;

    struct Fixture {
        dialogs: AttachDialogs,
        answers: Rc<RefCell<Vec<Value>>>,
        reports: Rc<RefCell<Vec<String>>>,
    }

    fn fixture() -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().to_string_lossy().into_owned();
        let keys = Arc::new(
            maho_core::keybindings::KeybindingsManager::create(Some(&agent_dir))
                .inner()
                .clone(),
        );
        let answers: Rc<RefCell<Vec<Value>>> = Rc::default();
        let reports: Rc<RefCell<Vec<String>>> = Rc::default();
        let (sent, shown) = (answers.clone(), reports.clone());
        let dialogs = AttachDialogs::new(
            &Theme::builtin("dark", ColorMode::Truecolor).unwrap(),
            keys,
            Rc::new(AttachEditorHost::new(30)),
            Box::new(move |response| sent.borrow_mut().push(response)),
            Box::new(move |message| shown.borrow_mut().push(message)),
        );
        Fixture {
            dialogs,
            answers,
            reports,
        }
    }

    fn select(id: &str) -> Value {
        json!({"type": "extension_ui_request", "method": "select", "id": id,
               "title": "Pick one", "options": ["first", "second"]})
    }

    #[test]
    fn answering_a_select_sends_the_chosen_value_and_dismisses_the_dialog() {
        let mut fixture = fixture();
        fixture.dialogs.accept(&select("r1"));
        assert!(fixture.dialogs.active());
        fixture.dialogs.handle_input("\r");
        assert!(!fixture.dialogs.active());
        let answers = fixture.answers.borrow();
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0]["id"], "r1");
        assert_eq!(answers[0]["value"], "first");
    }

    #[test]
    fn a_request_seen_twice_or_of_another_kind_is_shown_once_or_not_at_all() {
        let mut fixture = fixture();
        fixture.dialogs.accept(&select("r1"));
        fixture.dialogs.accept(&select("r1"));
        fixture.dialogs.accept(&select("r2"));
        fixture
            .dialogs
            .accept(&json!({"method": "notify", "id": "n1", "message": "hi"}));
        fixture.dialogs.handle_input("\r");
        assert!(fixture.dialogs.active());
        fixture.dialogs.handle_input("\r");
        assert!(!fixture.dialogs.active());
        assert_eq!(fixture.answers.borrow().len(), 2);
    }

    #[test]
    fn a_request_the_endpoint_settled_elsewhere_never_comes_back() {
        let mut fixture = fixture();
        fixture.dialogs.accept(&select("r1"));
        fixture.dialogs.accept(&select("r2"));
        fixture.dialogs.resolve("r2");
        fixture.dialogs.resolve("r1");
        assert!(!fixture.dialogs.active());
        assert!(fixture.answers.borrow().is_empty());
    }

    #[test]
    fn a_refused_answer_for_a_settled_request_stays_dismissed_and_says_why() {
        let mut fixture = fixture();
        fixture.dialogs.accept(&select("r1"));
        fixture.dialogs.handle_input("\r");
        let response_id = fixture.answers.borrow()[0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        fixture.dialogs.answer_failed(
            &response_id,
            &RequestError::AnswerRefused("question_already_resolved".to_owned()),
        );
        assert!(!fixture.dialogs.active());
        assert_eq!(
            fixture.reports.borrow().as_slice(),
            ["Not accepted: question_already_resolved (already settled)"]
        );
    }

    #[test]
    fn an_unacknowledged_answer_brings_the_dialog_back_to_answer_again() {
        let mut fixture = fixture();
        fixture.dialogs.accept(&select("r1"));
        fixture.dialogs.handle_input("\r");
        let response_id = fixture.answers.borrow()[0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        fixture.dialogs.answer_failed(
            &response_id,
            &RequestError::Failed("No acknowledgement for the answer".to_owned()),
        );
        assert!(fixture.dialogs.active());
        assert_eq!(
            fixture.reports.borrow().as_slice(),
            ["No acknowledgement for the answer. Answer again."]
        );
    }

    #[test]
    fn an_accepted_answer_is_forgotten() {
        let mut fixture = fixture();
        fixture.dialogs.accept(&select("r1"));
        fixture.dialogs.handle_input("\r");
        let response_id = fixture.answers.borrow()[0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        fixture.dialogs.answer_accepted(&response_id);
        fixture
            .dialogs
            .answer_failed(&response_id, &RequestError::Failed("late".to_owned()));
        assert!(!fixture.dialogs.active());
        assert!(fixture.reports.borrow().is_empty());
    }

    #[test]
    fn a_remote_device_is_named_without_its_control_characters() {
        let mut fixture = fixture();
        let mut request = select("r1");
        request["remoteOrigin"] = json!({"deviceId": "d1", "deviceName": "Pho\u{1b}[31mne\n"});
        fixture.dialogs.accept(&request);
        let text = fixture.dialogs.render(80).join("\n");
        assert!(text.contains("Requested from Pho[31mne"), "{text}");
        assert_eq!(
            remote_origin_label(&json!({"remoteOrigin": {"deviceName": "x"}})),
            None
        );
        assert_eq!(
            remote_origin_label(&json!({"remoteOrigin": {"deviceId": " id9 "}})).as_deref(),
            Some("Requested from id9")
        );
    }

    #[test]
    fn a_secret_input_is_never_shown_because_it_cannot_be_masked() {
        let mut fixture = fixture();
        fixture.dialogs.accept(&json!({
            "type": "extension_ui_request", "method": "input", "id": "s1",
            "title": "Token", "secret": true, "deadlineAtMs": u64::MAX,
        }));
        assert!(!fixture.dialogs.active());
        assert_eq!(fixture.reports.borrow().len(), 1);
    }

    #[test]
    fn a_confirm_answers_with_its_boolean_and_a_question_with_its_answers() {
        let mut fixture = fixture();
        fixture.dialogs.accept(&json!({
            "type": "extension_ui_request", "method": "confirm", "id": "c1",
            "title": "Proceed?", "message": "really",
        }));
        fixture.dialogs.handle_input("\r");
        assert_eq!(fixture.answers.borrow()[0]["confirmed"], true);
        let response = QuestionResponse {
            status: QuestionStatus::Answered,
            answers: std::iter::once((
                "q".to_owned(),
                maho_ext_api::QuestionAnswer {
                    selected: vec!["yes".to_owned()],
                    text: None,
                },
            ))
            .collect(),
            comment: Some("note".to_owned()),
            unanswered: Vec::new(),
            auto_resolved_after_ms: None,
            cancel_origin: None,
            resolved_by: None,
        };
        let reply = question_reply("q1", &response);
        assert_eq!(reply["answers"]["q"]["selected"][0], "yes");
        assert_eq!(reply["comment"], "note");
        let timed_out = QuestionResponse {
            status: QuestionStatus::TimedOut,
            ..response
        };
        assert_eq!(question_reply("q1", &timed_out)["cancelled"], true);
    }
}
