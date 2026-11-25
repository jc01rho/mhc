//! The parameters of `thread_create`.
//!
//! Upstream counterpart: `thread/contracts/create-params.ts` (`ThreadCreateParams`) and the unknown-field refusal of
//! `contracts/parse.ts` (`parseThreadParams`): a field the tool does not take is refused, naming the field meant, so a
//! brief passed as `message` is never dropped silently.

use serde_json::{Map, Value, json};

use super::contracts_permission_presets::PERMISSION_PRESETS;
use super::control_tools::THINKING_LEVELS;
use super::errors::{ThreadToolFailure, thread_tool_failure};
use super::worktree::thread_worktree::WorktreeRequest;

pub const THREAD_MESSAGE_MAX_BYTES: usize = 32_768;

const DELIVERY_MODES: [&str; 3] = ["auto", "steer", "follow_up"];

const KNOWN_FIELDS: [&str; 14] = [
    "thread",
    "device",
    "alias",
    "name",
    "cwd",
    "worktree",
    "fork_from",
    "skills",
    "model",
    "reasoning",
    "permission_preset",
    "prompt",
    "delivery",
    "idempotency_key",
];

/// The field a caller most likely meant by a name no thread tool takes, tried in order against the fields the tool does
/// take.
const MEANT: &[(&str, &[&str])] = &[
    ("message", &["prompt"]),
    ("prompt", &["message"]),
    ("text", &["message", "prompt"]),
    ("body", &["message", "prompt"]),
    ("content", &["message", "prompt"]),
    ("brief", &["prompt", "message"]),
    ("task", &["prompt", "message"]),
    ("instructions", &["prompt", "message"]),
    ("target", &["thread"]),
    ("thread_id", &["thread"]),
    ("session", &["thread"]),
    ("session_id", &["thread"]),
    ("id", &["thread"]),
    ("title", &["name"]),
    ("mode", &["delivery"]),
    ("level", &["reasoning"]),
    ("thinking", &["reasoning", "level"]),
    ("thinking_level", &["reasoning", "level"]),
    ("directory", &["cwd"]),
    ("folder", &["cwd"]),
    ("key", &["idempotency_key"]),
];

/// A `thread_create` call's arguments, checked against `ThreadCreateParams`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadCreateInput {
    pub thread: Option<String>,
    pub device: Option<String>,
    pub alias: Option<String>,
    pub name: Option<String>,
    pub cwd: Option<String>,
    pub worktree: Option<WorktreeRequest>,
    pub fork_from: Option<String>,
    pub skills: Option<Vec<String>>,
    pub model: Option<String>,
    pub reasoning: Option<String>,
    pub permission_preset: Option<String>,
    pub prompt: Option<String>,
    pub delivery: Option<String>,
    pub idempotency_key: Option<String>,
}

fn invalid_field(field: &str, message: &str) -> Box<ThreadToolFailure> {
    Box::new(thread_tool_failure(
        "invalid_arguments",
        format!("Parameter validation failed at /{field}: {message}"),
        "Fix the flagged field and call the tool again with a valid payload.",
        None,
    ))
}

fn unknown_field(field: &str) -> Box<ThreadToolFailure> {
    let suggestion: Option<&str> = MEANT
        .iter()
        .find(|(name, _)| *name == field)
        .and_then(|(_, meant)| {
            meant
                .iter()
                .find(|candidate| KNOWN_FIELDS.contains(candidate))
        })
        .copied();
    let message = match suggestion {
        Some(meant) => format!(
            "This tool takes no field \"{field}\"; did you mean \"{meant}\"? Nothing was done."
        ),
        None => format!("This tool takes no field \"{field}\". Nothing was done."),
    };
    let next_action = match suggestion {
        Some(meant) => format!("Call the tool again with \"{meant}\" in place of \"{field}\"."),
        None => format!(
            "Call the tool again without \"{field}\"; it takes: {}.",
            KNOWN_FIELDS.join(", ")
        ),
    };
    let mut details = Map::new();
    details.insert("field".to_owned(), json!(field));
    if let Some(meant) = suggestion {
        details.insert("suggestion".to_owned(), json!(meant));
    }
    details.insert("accepted".to_owned(), json!(KNOWN_FIELDS));
    Box::new(thread_tool_failure(
        "invalid_arguments",
        message,
        next_action,
        Some(Value::Object(details)),
    ))
}

fn text(
    object: &Map<String, Value>,
    field: &str,
    min: usize,
    max: usize,
) -> Result<Option<String>, Box<ThreadToolFailure>> {
    match object.get(field) {
        None => Ok(None),
        Some(Value::String(value)) => {
            let length = value.chars().count();
            if length < min {
                return Err(invalid_field(
                    field,
                    &format!("Expected string length greater or equal to {min}"),
                ));
            }
            if length > max {
                return Err(invalid_field(
                    field,
                    &format!("Expected string length less or equal to {max}"),
                ));
            }
            Ok(Some(value.clone()))
        }
        Some(_) => Err(invalid_field(field, "Expected string")),
    }
}

fn one_of(
    object: &Map<String, Value>,
    field: &str,
    allowed: &[&str],
) -> Result<Option<String>, Box<ThreadToolFailure>> {
    match object.get(field) {
        None => Ok(None),
        Some(Value::String(value)) if allowed.contains(&value.as_str()) => Ok(Some(value.clone())),
        Some(_) => Err(invalid_field(
            field,
            &format!("Expected one of: {}", allowed.join(", ")),
        )),
    }
}

fn skills(object: &Map<String, Value>) -> Result<Option<Vec<String>>, Box<ThreadToolFailure>> {
    match object.get("skills") {
        None => Ok(None),
        Some(Value::Array(items)) => {
            if items.is_empty() {
                return Err(invalid_field(
                    "skills",
                    "Expected array length greater or equal to 1",
                ));
            }
            if items.len() > 16 {
                return Err(invalid_field(
                    "skills",
                    "Expected array length less or equal to 16",
                ));
            }
            let mut names = Vec::new();
            for (index, item) in items.iter().enumerate() {
                let path = format!("skills/{index}");
                let Value::String(name) = item else {
                    return Err(invalid_field(&path, "Expected string"));
                };
                if !(1..=64).contains(&name.chars().count()) {
                    return Err(invalid_field(
                        &path,
                        "Expected string length between 1 and 64",
                    ));
                }
                names.push(name.clone());
            }
            Ok(Some(names))
        }
        Some(_) => Err(invalid_field("skills", "Expected array")),
    }
}

fn worktree(
    object: &Map<String, Value>,
) -> Result<Option<WorktreeRequest>, Box<ThreadToolFailure>> {
    match object.get("worktree") {
        None => Ok(None),
        Some(Value::Object(inner)) => {
            if let Some(extra) = inner
                .keys()
                .find(|key| key.as_str() != "branch" && key.as_str() != "base")
            {
                return Err(invalid_field(
                    &format!("worktree/{extra}"),
                    "Unexpected property",
                ));
            }
            Ok(Some(WorktreeRequest {
                branch: text(inner, "branch", 1, 200)?,
                base: text(inner, "base", 1, 200)?,
            }))
        }
        Some(_) => Err(invalid_field("worktree", "Expected object")),
    }
}

impl ThreadCreateInput {
    /// A field the tool does not take is refused first, named; then each field is checked against its schema.
    pub fn parse(args: &Value) -> Result<Self, Box<ThreadToolFailure>> {
        let Some(object) = args.as_object() else {
            return Err(invalid_field("", "Expected object"));
        };
        if let Some(field) = object
            .keys()
            .find(|key| !KNOWN_FIELDS.contains(&key.as_str()))
        {
            return Err(unknown_field(field));
        }
        Ok(Self {
            thread: text(object, "thread", 1, usize::MAX)?,
            device: text(object, "device", 1, usize::MAX)?,
            alias: text(object, "alias", 1, 64)?,
            name: text(object, "name", 0, usize::MAX)?,
            cwd: text(object, "cwd", 0, usize::MAX)?,
            worktree: worktree(object)?,
            fork_from: text(object, "fork_from", 0, usize::MAX)?,
            skills: skills(object)?,
            model: text(object, "model", 3, usize::MAX)?,
            reasoning: one_of(object, "reasoning", &THINKING_LEVELS)?,
            permission_preset: one_of(object, "permission_preset", PERMISSION_PRESETS)?,
            prompt: text(object, "prompt", 1, THREAD_MESSAGE_MAX_BYTES)?,
            delivery: one_of(object, "delivery", &DELIVERY_MODES)?,
            idempotency_key: text(object, "idempotency_key", 0, usize::MAX)?,
        })
    }
}

fn property(kind: &str, description: &str) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("type".to_owned(), json!(kind));
    map.insert("description".to_owned(), json!(description));
    map
}

fn string_property(description: &str, min: Option<u64>, max: Option<u64>) -> Value {
    let mut map = property("string", description);
    if let Some(min) = min {
        map.insert("minLength".to_owned(), json!(min));
    }
    if let Some(max) = max {
        map.insert("maxLength".to_owned(), json!(max));
    }
    Value::Object(map)
}

fn enum_property(description: &str, values: &[&str]) -> Value {
    let mut map = property("string", description);
    map.insert("enum".to_owned(), json!(values));
    Value::Object(map)
}

fn worktree_property() -> Value {
    let mut properties = Map::new();
    properties.insert(
        "branch".to_owned(),
        string_property(
            "New branch the worktree checks out; must not exist yet. Default: omo/thread-<time>.",
            Some(1),
            Some(200),
        ),
    );
    properties.insert(
        "base".to_owned(),
        string_property(
            "Commit, branch or tag the new branch starts at. Default: the repository's HEAD.",
            Some(1),
            Some(200),
        ),
    );
    let mut map = property(
        "object",
        "Create a new git worktree for the new thread and start the thread in it, on the device the thread is created on: from the repository at cwd (the caller's folder when cwd is unset; required with device), on a new branch, in OmO's worktrees folder on that device. The result's thread.worktree names its path, branch and base commit. A folder that is not a git repository, a branch that exists, a path that exists, no disk space or no permission each refuse the create with a typed code and details.reason; nothing is opened then. Only for a new thread.",
    );
    map.insert("properties".to_owned(), Value::Object(properties));
    map.insert("additionalProperties".to_owned(), json!(false));
    Value::Object(map)
}

fn skills_property() -> Value {
    let mut map = property(
        "array",
        "Names of installed skills the new thread follows from its first turn, as if each were loaded with /skill. A path or text that is not a skill name refuses the create. The new session applies each name its own skill registry holds and logs any it does not; the result's thread.requested_skills echoes the names passed.",
    );
    map.insert(
        "items".to_owned(),
        json!({ "type": "string", "minLength": 1, "maxLength": 64 }),
    );
    map.insert("minItems".to_owned(), json!(1));
    map.insert("maxItems".to_owned(), json!(16));
    Value::Object(map)
}

/// The JSON schema of `thread_create`'s arguments (`ThreadCreateParams`).
pub fn thread_create_parameters() -> Value {
    let mut properties = Map::new();
    let mut put = |name: &str, schema: Value| {
        properties.insert(name.to_owned(), schema);
    };
    put(
        "thread",
        string_property(
            "Id or alias of an EXISTING thread to open, on whichever of the account's devices holds it; leave unset to create a new thread. Never combined with device.",
            Some(1),
            None,
        ),
    );
    put(
        "device",
        string_property(
            "Account device to create the new thread on (from thread_list with all_scope, which is tool.thread_list({ all_scope: true }) in an eval cell); leave unset to create it on this device. Only for a new thread.",
            Some(1),
            None,
        ),
    );
    put(
        "alias",
        string_property(
            "Account-wide alias for the new thread, usable instead of its id in every thread tool; an alias another thread holds returns alias_taken naming the holder. With no signed-in account it becomes the thread's local name instead (unless name is set) and the result carries alias_scope \"local\".",
            Some(1),
            Some(64),
        ),
    );
    put(
        "name",
        string_property(
            "Display label for the new thread; an existing thread with the same normalized name returns name_conflict, and later renames change only the label while the thread value stays the address.",
            None,
            None,
        ),
    );
    put(
        "cwd",
        string_property(
            "Working directory the new thread runs in; leaving it unset keeps the thread inside the caller's workspace. A relative path resolves against the caller's folder on this device; on another device pass an absolute path of that device (thread_list with all_scope, which is tool.thread_list({ all_scope: true }) in an eval cell, shows its threads' folders). A folder that does not exist refuses the create with invalid_arguments.",
            None,
            None,
        ),
    );
    put("worktree", worktree_property());
    put(
        "fork_from",
        string_property(
            "Durable id of an existing thread to fork. The engine does not fork on open yet (#13031), so this is refused with unsupported (details.reason fork_unsupported) and nothing is created; create a thread and hand it the context with thread_send (tool.thread_send() in an eval cell) instead.",
            None,
            None,
        ),
    );
    put("skills", skills_property());
    put(
        "model",
        string_property(
            "Model the new thread runs on, as an exact provider/id of the device it is created on (listed by thread_set_model, which is tool.thread_set_model() in an eval cell); the session is created on it, and a model that device cannot run refuses the create with model_not_found and opens nothing. Default: that device's own default. Only for a new thread.",
            Some(3),
            None,
        ),
    );
    put(
        "reasoning",
        enum_property(
            "Thinking level the new thread starts with. Default: that device's own default. Only for a new thread.",
            &THINKING_LEVELS,
        ),
    );
    put(
        "permission_preset",
        enum_property(
            "Approval mode the new thread runs in: this session's own mode or one that allows less (full-access > workspace > accept-edits > read-only > ask; auto sits between full-access and ask only). A wider mode refuses the create with scope_denied. Default: this session's own mode. Only for a new thread.",
            PERMISSION_PRESETS,
        ),
    );
    put(
        "prompt",
        string_property(
            &format!(
                "First message for the new thread, delivered as thread_send (tool.thread_send() in an eval cell) delivers it once the thread exists (delivery picks its mode); the result's prompt field names that delivery, or its failure while the thread stays created. Covered by the same idempotency_key as the create. Also sent when thread opens an existing thread: it goes to that thread (#14335). Longer than {THREAD_MESSAGE_MAX_BYTES} UTF-8 bytes returns message_too_large."
            ),
            Some(1),
            Some(THREAD_MESSAGE_MAX_BYTES as u64),
        ),
    );
    put(
        "delivery",
        enum_property(
            "Omitted: auto. auto steers the running turn when one is active and starts a new turn otherwise; steer requires expected_turn_id and becomes turn_conflict when the active turn changed; follow_up queues the message behind the running turn (pass it only when the message must wait for that turn to end).",
            &DELIVERY_MODES,
        ),
    );
    put(
        "idempotency_key",
        string_property(
            "Caller-chosen key that makes a retried call return the earlier result instead of repeating the side effect; reusing the key with different arguments returns idempotency_conflict.",
            None,
            None,
        ),
    );
    json!({
        "type": "object",
        "properties": Value::Object(properties),
        "additionalProperties": false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_field_the_schema_names_parses() {
        let parsed = ThreadCreateInput::parse(&json!({
            "name": "builder",
            "cwd": "/work",
            "worktree": { "branch": "feat/x" },
            "skills": ["programming"],
            "model": "anthropic/opus",
            "reasoning": "high",
            "permission_preset": "ask",
            "prompt": "go",
            "delivery": "follow_up",
            "idempotency_key": "k1",
            "alias": "bee",
        }))
        .expect("parses");
        assert_eq!(parsed.name.as_deref(), Some("builder"));
        assert_eq!(
            parsed.worktree,
            Some(WorktreeRequest {
                branch: Some("feat/x".into()),
                base: None
            })
        );
        assert_eq!(parsed.skills, Some(vec!["programming".to_owned()]));
        assert_eq!(parsed.delivery.as_deref(), Some("follow_up"));
        assert_eq!(parsed.thread, None);
        assert_eq!(
            ThreadCreateInput::parse(&json!({})),
            Ok(ThreadCreateInput::default())
        );
    }

    #[test]
    fn the_properties_of_the_schema_are_the_fields_the_parser_takes() {
        let schema = thread_create_parameters();
        let mut names: Vec<&str> = schema["properties"]
            .as_object()
            .expect("properties")
            .keys()
            .map(String::as_str)
            .collect();
        names.sort_unstable();
        let mut known = KNOWN_FIELDS.to_vec();
        known.sort_unstable();
        assert_eq!(names, known);
        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn a_brief_passed_as_message_is_refused_naming_prompt() {
        let refused = ThreadCreateInput::parse(&json!({ "name": "x", "message": "build it" }))
            .expect_err("refused");
        assert_eq!(refused.code.as_str(), "invalid_arguments");
        let details = refused.details.expect("details");
        assert_eq!(details["field"], "message");
        assert_eq!(details["suggestion"], "prompt");
        assert_eq!(details["accepted"].as_array().map(Vec::len), Some(14));
        assert!(refused.message.contains("did you mean \"prompt\""));
    }

    #[test]
    fn an_unknown_field_with_no_suggestion_lists_what_the_tool_takes() {
        let refused =
            ThreadCreateInput::parse(&json!({ "priority": "high" })).expect_err("refused");
        let details = refused.details.expect("details");
        assert_eq!(details["field"], "priority");
        assert!(details.get("suggestion").is_none());
        assert!(refused.next_action.contains("it takes: thread, device"));
    }

    #[test]
    fn a_field_of_the_wrong_shape_names_its_path() {
        for (args, path) in [
            (json!({ "prompt": "" }), "/prompt"),
            (json!({ "prompt": 3 }), "/prompt"),
            (json!({ "alias": "a".repeat(65) }), "/alias"),
            (json!({ "model": "ab" }), "/model"),
            (json!({ "reasoning": "extreme" }), "/reasoning"),
            (json!({ "permission_preset": "yolo" }), "/permission_preset"),
            (json!({ "delivery": "later" }), "/delivery"),
            (json!({ "skills": [] }), "/skills"),
            (json!({ "skills": [1] }), "/skills/0"),
            (json!({ "worktree": { "force": true } }), "/worktree/force"),
            (json!({ "thread": "" }), "/thread"),
        ] {
            let refused = ThreadCreateInput::parse(&args).expect_err("refused");
            assert_eq!(refused.code.as_str(), "invalid_arguments", "{args}");
            assert!(
                refused.message.contains(&format!("at {path}:")),
                "{args}: {}",
                refused.message
            );
        }
        assert_eq!(
            ThreadCreateInput::parse(&Value::Null)
                .expect_err("not an object")
                .code
                .as_str(),
            "invalid_arguments"
        );
    }
}
