//! The Fable 5.5 launch gate is retired (upstream `89d40745ba`): no routing table names a
//! `claude-fable-*` model. What stays is the capability placeholder row the snapshot layers use.

use model_core::FABLE_55_LAUNCH;
use model_core::FABLE_55_LAUNCH_ID;
use model_core::latest_models::LATEST_MODEL_BY_FAMILY;
use pretty_assertions::assert_eq;

#[test]
fn the_launch_row_stays_a_capability_only_placeholder() {
    assert_eq!(FABLE_55_LAUNCH.id, FABLE_55_LAUNCH_ID);
    assert_eq!(FABLE_55_LAUNCH.family.as_deref(), Some("claude-fable"));
    assert_eq!(FABLE_55_LAUNCH.reasoning, None);
    assert_eq!(FABLE_55_LAUNCH.tool_call, None);
    assert_eq!(FABLE_55_LAUNCH.modalities, None);
    let limit = FABLE_55_LAUNCH.limit.as_ref().expect("placeholder limit");
    assert_eq!(
        (limit.context, limit.input, limit.output),
        (None, None, None)
    );
}

#[test]
fn the_latest_model_table_does_not_name_the_launch_id() {
    let claude_fable = LATEST_MODEL_BY_FAMILY
        .iter()
        .find(|(family, _)| *family == "claudeFable")
        .map(|(_, model)| *model);
    assert_ne!(claude_fable, Some(FABLE_55_LAUNCH_ID));
}
