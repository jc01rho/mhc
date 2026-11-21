use maho_omo_remote::args::{Delivery, SessionArgs, Target, WakeValue, parse};
use maho_omo_remote::command::command_argv;
use maho_omo_remote::exit::UsageError;
use maho_omo_remote::rpc_host::{action_of, host_session, outcome_of};
use maho_omo_remote::usage::{remote_usage, verb_usage};
use maho_rpc::host_runner::HostOutcome;
use maho_rpc::host_session_runner::HostSessionAction;
use mhc_relayd::HostCommandOutcome;
use serde_json::json;

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

#[test]
fn session_verbs_parse_their_targets_and_options() {
    let send = parse(&argv(&["send", "laptop/s1", "--steer", "--", "--hello"])).expect("send");
    assert_eq!(
        send.session,
        Some(SessionArgs::Send {
            target: Target {
                device: Some("laptop".into()),
                session: "s1".into(),
            },
            text: "--hello".into(),
            delivery: Delivery::Steer,
            wait: true,
        })
    );
    let close = parse(&argv(&["close", "durable-1", "--force", "--json"])).expect("close");
    assert!(close.json);
    assert_eq!(
        close.session,
        Some(SessionArgs::Close {
            target: Target {
                device: None,
                session: "durable-1".into(),
            },
            force: true,
        })
    );
    let model = parse(&argv(&["model", "box/s2", "openai/gpt-5"])).expect("model");
    assert!(
        matches!(model.session, Some(SessionArgs::Model { ref model, .. }) if model == "openai/gpt-5")
    );
}

#[test]
fn bad_arguments_are_usage_errors() {
    for words in [
        vec!["send", "/s1", "hi"],
        vec!["send", "laptop/", "hi"],
        vec!["send", "laptop/s1"],
        vec!["send", "laptop/s1", "hi", "--steer", "--follow-up"],
        vec!["send", "laptop/s1", "hi", "--timeout", "601"],
        vec!["model", "laptop/s1", "gpt-5"],
        vec!["close", "laptop/s1", "--soon"],
        vec!["control", "maybe"],
        vec!["wake", "on", "off"],
        vec!["sessions", "--state", "sleeping"],
        vec!["sessions", "--device"],
        vec!["devices", "--all"],
    ] {
        assert!(
            matches!(parse(&argv(&words)), Err(UsageError(_))),
            "{words:?} is a usage error"
        );
    }
}

#[test]
fn switches_parse() {
    assert_eq!(
        parse(&argv(&["control", "off"])).expect("control").control,
        Some(false)
    );
    assert_eq!(
        parse(&argv(&["wake", "default"])).expect("wake").wake,
        Some(WakeValue::Default)
    );
    let sessions =
        parse(&argv(&["sessions", "--device", "box", "--state", "idle"])).expect("sessions");
    assert_eq!(sessions.filter.device.as_deref(), Some("box"));
    assert_eq!(sessions.filter.state.as_deref(), Some("idle"));
}

#[test]
fn usage_lists_every_verb_and_each_verb_has_its_entry() {
    let usage = remote_usage();
    for verb in [
        "status",
        "devices",
        "sessions",
        "enable",
        "disable",
        "control",
        "wake",
        "bootstrap",
        "daemon",
        "attach",
        "open",
        "send",
        "model",
        "interrupt",
        "close",
    ] {
        assert!(usage.contains(verb), "{verb} in the usage");
        assert!(
            verb_usage(verb)
                .lines()
                .nth(1)
                .is_some_and(|line| line.contains(verb))
        );
    }
}

#[test]
fn the_remote_command_keeps_a_sent_message_whole() {
    assert_eq!(
        command_argv("send laptop/s1 hello there  world"),
        argv(&["send", "laptop/s1", "hello there  world"])
    );
    assert_eq!(
        command_argv("  sessions --device box "),
        argv(&["sessions", "--device", "box"])
    );
    assert!(command_argv("   ").is_empty());
}

#[test]
fn host_rows_and_records_map_to_host_actions_and_outcomes() {
    let row = json!({"sessionId": "rpc-1", "durableSessionId": "d1", "cwd": "/w", "name": "alpha", "sessionPath": "/w/s.jsonl", "status": "open", "kind": "interactive"});
    let session = host_session(&row).expect("row");
    assert_eq!(session.session_id, "rpc-1");
    assert!(session.is_named("d1"));
    assert_eq!(session.state, "idle");
    assert_eq!(session.project.as_deref(), Some("/w"));
    assert_eq!(
        host_session(&json!({"status": "closing", "sessionId": "x"}))
            .expect("row")
            .state,
        "ended"
    );

    assert!(matches!(
        action_of("rpc-1", &json!({"type": "follow_up", "message": "hi"})),
        Some(HostSessionAction::Prompt { ref text, .. }) if text == "hi"
    ));
    assert!(matches!(
        action_of("rpc-1", &json!({"type": "set_model", "provider": "openai", "modelId": "gpt-5"})),
        Some(HostSessionAction::Model { ref model, .. }) if model.provider == "openai" && model.id == "gpt-5"
    ));
    assert!(action_of("rpc-1", &json!({"type": "bash"})).is_none());
    assert!(action_of("rpc-1", &json!({"type": "set_model", "provider": "openai"})).is_none());

    let refused = |reason: &str| HostOutcome {
        exit_code: 3,
        payload: json!({ "reason": reason }),
    };
    assert_eq!(
        outcome_of(refused("unknown_session")),
        HostCommandOutcome::NotListed
    );
    assert_eq!(
        outcome_of(refused("host_stopping")),
        HostCommandOutcome::Refused("host_stopping".into())
    );
    assert!(matches!(
        outcome_of(HostOutcome {
            exit_code: 0,
            payload: json!({"disposition": "started"})
        }),
        HostCommandOutcome::Applied(_)
    ));
    assert!(matches!(
        outcome_of(HostOutcome {
            exit_code: 1,
            payload: json!({})
        }),
        HostCommandOutcome::Uncertain(_)
    ));
}
