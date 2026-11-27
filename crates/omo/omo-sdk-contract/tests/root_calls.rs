//! Port of oh-my-openagent `script/sdk-parity/contract.test.ts` and `contract-controls.test.ts` (v5.1.84, slices P1a
//! #14948 and P1a-s #14949) over the root's typed call table: every row's input schema, its answer shape, the
//! wait-options rule, and the cases the TS suite pins by hand. The TS cases that read the registry rows, the scenario
//! files, the security design document, the engine snapshot or a running JS host stay with the TS parity tooling.

use omo_sdk_contract::calls::{
    AnswerShape, ROOT_CALLS, answer_shape, call, calls_of, can_wait, input_accepts,
    input_omittable, is_root_call, namespace_of, takes_deadline_option,
};
use omo_sdk_contract::namespaces::is_namespace;
use omo_sdk_contract::results::root::{answer_accepts, answer_spec};
use omo_sdk_contract::shape::{Shape, accepts, all_fields_optional, names_field, sample};
use serde_json::{Value, json};

fn keys() -> Vec<&'static str> {
    ROOT_CALLS.iter().map(|call| call.key).collect()
}

/// A value that is not inside `shape`, built from a sample: the first required field changed to a value of another
/// JSON type, a required field removed, or an unknown field added.
fn refusals(shape: &Shape) -> Vec<Value> {
    let mut out = Vec::new();
    let Some(base) = sample(shape) else {
        return out;
    };
    if let (Some(object), Shape::Object { fields, .. }) = (base.as_object(), shape) {
        if let Some(field) = fields.iter().find(|field| !field.optional) {
            let mut changed = object.clone();
            changed.insert(field.name.to_owned(), json!([]));
            out.push(Value::Object(changed));
            let mut removed = object.clone();
            removed.remove(field.name);
            out.push(Value::Object(removed));
        }
        let mut extra = object.clone();
        extra.insert("not_a_field".to_owned(), json!(1));
        out.push(Value::Object(extra));
    }
    out.push(json!("not an input"));
    out.push(json!([]));
    out
}

#[test]
fn the_table_has_one_call_per_row_key_and_no_duplicate() {
    let mut seen = std::collections::BTreeSet::new();
    for call in ROOT_CALLS {
        assert!(seen.insert(call.key), "duplicate call {}", call.key);
    }
    assert_eq!(seen.len(), ROOT_CALLS.len());
    assert_eq!(ROOT_CALLS.len(), 134);
}

#[test]
fn every_call_key_is_a_dotted_path_in_a_namespace_of_the_design() {
    for key in keys() {
        assert!(key.split('.').count() >= 2, "{key} is no dotted call");
        assert!(is_namespace(namespace_of(key)), "{key} has no namespace");
        assert!(is_root_call(key));
        assert_eq!(call(key).map(|call| call.key), Some(key));
    }
    assert!(!is_root_call("threads.nothing"));
    assert!(!input_accepts("threads.nothing", &json!({})));
    assert!(answer_shape("threads.nothing").is_none());
    assert!(!can_wait("threads.nothing"));
    assert!(!takes_deadline_option("threads.nothing"));
}

#[test]
fn every_call_has_an_input_it_accepts_and_inputs_it_refuses() {
    for call in ROOT_CALLS {
        if no_json_form(call.key) {
            assert!(
                sample(&call.input).is_none_or(|base| !input_accepts(call.key, &base)),
                "{} has no JSON form",
                call.key
            );
            continue;
        }
        if let Some(base) = sample(&call.input) {
            assert!(
                input_accepts(call.key, &base),
                "{} refuses its own sample {base}",
                call.key
            );
        }
        let refused = refusals(&call.input)
            .into_iter()
            .filter(|value| !accepts(&call.input, value))
            .count();
        assert!(refused > 0, "{} refuses nothing but any value", call.key);
    }
}

/// The calls whose input is a handle or a callback: `account.adopt` and `account.signIn`, `sync.enable` and
/// `sync.recover` (security design 5.3). Their authority is `custody_input_accepts`, which admits no JSON value.
fn no_json_form(key: &str) -> bool {
    matches!(
        key,
        "account.signIn" | "account.adopt" | "sync.enable" | "sync.recover"
    )
}

#[test]
fn a_call_whose_input_carries_a_handle_has_no_json_form() {
    for key in [
        "account.signIn",
        "account.adopt",
        "sync.enable",
        "sync.recover",
    ] {
        assert!(no_json_form(key));
        assert!(
            !input_accepts(key, &json!({})),
            "{key} must refuse an empty object"
        );
        if let Some(spec) = call(key)
            && let Some(base) = sample(&spec.input)
        {
            assert!(!input_accepts(key, &base), "{key} must refuse {base}");
        }
    }
    // A launch call's api_key is a handle too, but optional: a complete JSON input that names one is refused, while an
    // input that leaves it out is inside the schema.
    for key in [
        "engine.launch.interactive",
        "engine.launch.print",
        "engine.launch.rpc",
        "engine.launch.hand",
        "engine.launch.headless",
    ] {
        let with_key = json!({ "api_key": { "handle": 1 } });
        assert!(
            !input_accepts(key, &with_key),
            "{key} admits a JSON api_key"
        );
        assert!(!input_accepts(key, &json!({ "api_key": "text" })));
    }
    assert!(input_accepts("engine.launch.interactive", &json!({})));
    assert!(!input_accepts("engine.launch.print", &json!({})));
    // `signal` is a handle too: TypeBox types it `{}`, which admits any JSON value, but a handle is no JSON value.
    for key in ["engine.schedule.run", "threads.runToCompletion"] {
        let mut input = if key == "engine.schedule.run" {
            json!({})
        } else {
            json!({ "message": "m" })
        };
        assert!(input_accepts(key, &input), "{key} refuses its core input");
        for signal in [json!({}), json!([]), json!("s"), json!(1), json!(null)] {
            input["signal"] = signal.clone();
            assert!(
                !input_accepts(key, &input),
                "{key} admits the JSON signal {signal}"
            );
        }
    }
}

#[test]
fn only_a_waitable_call_takes_the_deadline_option_and_only_when_its_input_lacks_it() {
    let waiting: Vec<&str> = ROOT_CALLS
        .iter()
        .filter(|call| call.waits)
        .map(|call| call.key)
        .collect();
    assert!(!waiting.is_empty());
    for call in ROOT_CALLS {
        assert_eq!(
            takes_deadline_option(call.key),
            call.waits && !names_field(&call.input, "deadline_ms"),
            "{}",
            call.key
        );
    }
    // The three calls whose input already names deadline_ms take only the signal.
    let names_deadline: Vec<&str> = ROOT_CALLS
        .iter()
        .filter(|call| names_field(&call.input, "deadline_ms"))
        .map(|call| call.key)
        .collect();
    assert_eq!(
        names_deadline,
        ["threads.wait", "threads.sendAndFollow", "daemon.stopAll"]
    );
    for key in names_deadline {
        assert!(can_wait(key));
        assert!(!takes_deadline_option(key));
    }
    // A call that answers at once from this process takes no wait options at all.
    for key in [
        "account.signInCheck",
        "daemon.shardPath",
        "install.version",
        "install.sdkRoot",
        "toolkit.help",
        "engine.config.agentDir",
        "engine.tips",
    ] {
        assert!(!can_wait(key), "{key} answers at once");
        assert!(!takes_deadline_option(key), "{key} takes no options");
    }
}

#[test]
fn every_answer_is_one_of_the_three_shapes_of_design_four_four() {
    for call in ROOT_CALLS {
        assert!(matches!(
            call.answer,
            AnswerShape::Value | AnswerShape::Thread | AnswerShape::Stream
        ));
    }
    let streams: Vec<&str> = ROOT_CALLS
        .iter()
        .filter(|call| call.answer == AnswerShape::Stream)
        .map(|call| call.key)
        .collect();
    assert_eq!(
        streams,
        [
            "threads.follow",
            "threads.sendAndFollow",
            "threads.runToCompletion",
            "engine.launch.print",
            "engine.schedule.run",
        ]
    );
    // A stream can wait; a thread call answers the thread contract's own ok branch.
    for key in streams {
        assert!(can_wait(key));
    }
}

#[test]
fn an_input_is_omittable_exactly_when_every_field_is_optional() {
    for call in ROOT_CALLS {
        assert_eq!(
            input_omittable(call.key),
            all_fields_optional(&call.input),
            "{}",
            call.key
        );
    }
    // install.setup.apply takes a required plan, so its input is not omittable (design 6.7).
    assert!(!input_omittable("install.setup.apply"));
    assert!(input_omittable("threads.list"));
    assert!(input_omittable("daemon.run"));
    // A union is omittable only when every member is.
    assert!(!input_omittable("engine.config.setResource"));
    assert!(!input_omittable("install.plugin"));
}

#[test]
fn threads_create_takes_a_string_prompt_or_the_object_form_never_empty() {
    assert!(input_accepts("threads.create", &json!({ "prompt": "hi" })));
    assert!(input_accepts(
        "threads.create",
        &json!({ "prompt": { "message": "hi", "delivery": "steer", "expected_turn_id": "t1" } })
    ));
    assert!(!input_accepts("threads.create", &json!({ "prompt": {} })));
    assert!(!input_accepts(
        "threads.create",
        &json!({ "prompt": { "delivery": "steer" } })
    ));
    assert!(!input_accepts("threads.create", &json!({ "prompt": "" })));
}

#[test]
fn only_bytes_are_no_attachment_and_text_or_the_tables_fixture_reference_is_refused() {
    let send = |bytes: Value| {
        json!({
            "thread": "t",
            "message": "m",
            "attachments": [{ "bytes": bytes, "name": "a.txt" }],
        })
    };
    assert!(!input_accepts("threads.send", &send(json!("aGk="))));
    assert!(!input_accepts("threads.send", &send(json!([104, 105]))));
    assert!(!input_accepts(
        "threads.send",
        &send(json!({
            "$bytes": {
                "fixture": "hi",
                "length": 2,
                "sha256": "8f434346648f6b96df89dda901c5176b10a6d83961dd3c1ac88b59b2dc327aa4",
            },
        }))
    ));
    // A path attachment is the other admitted form.
    assert!(input_accepts(
        "threads.send",
        &json!({ "thread": "t", "message": "m", "attachments": [{ "path": "a.txt" }] })
    ));
    assert!(!input_accepts(
        "threads.send",
        &json!({ "thread": "t", "message": "m", "attachments": [] })
    ));
}

#[test]
fn install_setup_apply_takes_the_plan_install_setup_plan_answers() {
    let plan = json!({
        "inventory": { "harnesses": [] },
        "stages": [{ "id": "import-credentials", "description": "import" }],
    });
    assert!(input_accepts(
        "install.setup.apply",
        &json!({ "consent": "all", "plan": plan })
    ));
    assert!(!input_accepts(
        "install.setup.apply",
        &json!({ "consent": "each" })
    ));
    assert!(!input_accepts(
        "install.setup.apply",
        &json!({ "consent": "all", "plan": { "stages": "all" } })
    ));
    assert!(!input_accepts(
        "install.setup.apply",
        &json!({ "consent": "some", "plan": plan })
    ));
}

#[test]
fn engine_config_set_resource_takes_exactly_the_global_or_the_project_local_form() {
    let base = json!({
        "source": "npm:pkg",
        "resource": { "type": "skill", "path": "skills/a" },
    });
    let with = |extra: Value| {
        let mut input = base.clone();
        for (key, value) in extra.as_object().expect("an object") {
            input[key] = value.clone();
        }
        input
    };
    assert!(input_accepts(
        "engine.config.setResource",
        &with(json!({ "enabled": false }))
    ));
    assert!(input_accepts(
        "engine.config.setResource",
        &with(json!({ "trust": true, "enabled": true }))
    ));
    assert!(input_accepts(
        "engine.config.setResource",
        &with(json!({ "local": true, "state": "inherit" }))
    ));
    assert!(!input_accepts("engine.config.setResource", &base));
    assert!(!input_accepts(
        "engine.config.setResource",
        &with(json!({ "enabled": true, "state": "load" }))
    ));
    assert!(!input_accepts(
        "engine.config.setResource",
        &with(json!({ "local": true, "enabled": true }))
    ));
    assert!(!input_accepts(
        "engine.config.setResource",
        &with(json!({ "state": "unload" }))
    ));
    assert!(!input_accepts(
        "engine.config.setResource",
        &with(json!({ "local": false, "state": "load" }))
    ));
}

#[test]
fn every_namespace_of_the_design_has_at_least_one_call() {
    for namespace in [
        "threads",
        "devices",
        "account",
        "deviceService",
        "sync",
        "config",
        "daemon",
        "diagnostics",
        "install",
        "engine",
        "mcp",
        "boulder",
        "worktrees",
        "toolkit",
    ] {
        assert!(
            !calls_of(namespace).is_empty(),
            "namespace {namespace} has no call"
        );
    }
    assert!(calls_of("nothing").is_empty());
    assert_eq!(namespace_of("deviceService.setControl"), "deviceService");
}

#[test]
fn an_answer_record_admits_its_sample_and_refuses_a_field_change() {
    let mut checked = 0;
    for call in ROOT_CALLS {
        if call.answer == AnswerShape::Stream {
            continue;
        }
        let Some(shape) = answer_spec(call.key) else {
            continue;
        };
        checked += 1;
        if let Some(base) = sample(shape) {
            assert!(
                answer_accepts(call.key, &base),
                "{} refuses its own answer sample {base}",
                call.key
            );
            if let (Some(object), Shape::Object { fields, .. }) = (base.as_object(), shape)
                && let Some(field) = fields.iter().find(|field| !field.optional)
            {
                let mut changed = object.clone();
                changed.insert(field.name.to_owned(), json!({ "not": "the field" }));
                assert!(
                    !answer_accepts(call.key, &Value::Object(changed)),
                    "{} admits a changed {}",
                    call.key,
                    field.name
                );
            }
        }
    }
    assert!(
        checked > 40,
        "only {checked} answers carry a restated record"
    );
}

#[test]
fn a_custody_answer_is_the_closed_record_the_custody_slice_carries() {
    // The table defers to the custody slice for these calls; this pins that the deference is real.
    assert!(answer_spec("account.signOut").is_some());
    assert!(answer_spec("account.credentials.handOver").is_none());
    assert!(answer_accepts(
        "account.signOut",
        &json!({ "signedOut": true, "revoked": false })
    ));
    assert!(!answer_accepts(
        "account.signOut",
        &json!({ "signedOut": true })
    ));
    assert!(!answer_accepts(
        "account.credentials.handOver",
        &json!({ "delivered": true })
    ));
    assert!(!answer_accepts("threads.nothing", &json!({})));
}
