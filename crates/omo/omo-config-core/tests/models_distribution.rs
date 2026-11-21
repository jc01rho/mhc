use omo_config_core::ModelDistributor;
use serde_json::{Value, json};

#[test]
fn default_and_sequential_leave_the_chain_and_the_cursor_alone() {
    let distributor = ModelDistributor::new();
    let entries = vec![
        json!("provider/a"),
        json!("provider/b"),
        json!("provider/c"),
    ];
    assert_eq!(
        distributor.distribute("category:one", &entries, None),
        entries
    );
    assert_eq!(
        distributor.distribute("category:one", &entries, Some("sequential")),
        entries
    );
    assert_eq!(
        distributor.distribute("category:one", &entries, Some("round_robin")),
        entries
    );
    assert_eq!(
        distributor.distribute("category:one", &entries, Some("round_robin")),
        vec![
            json!("provider/b"),
            json!("provider/c"),
            json!("provider/a")
        ]
    );
    assert_eq!(
        entries,
        vec![
            json!("provider/a"),
            json!("provider/b"),
            json!("provider/c")
        ]
    );
}

#[test]
fn cursors_are_per_target_and_owner_and_wrap_without_duplicating_models() {
    let distributor = ModelDistributor::new();
    let entries = vec![
        json!("provider/a"),
        json!({ "model": "provider/b", "reasoning": "high" }),
        json!("provider/a:low"),
    ];
    let first_two = entries[..2].to_vec();
    assert_eq!(
        distributor.distribute("category:one", &entries, Some("round_robin")),
        first_two
    );
    assert_eq!(
        distributor.distribute("category:one", &entries, Some("round_robin")),
        vec![entries[1].clone(), entries[0].clone()]
    );
    assert_eq!(
        distributor.distribute("agent:one", &entries, Some("round_robin")),
        first_two
    );
    assert_eq!(
        distributor.distribute("category:two", &entries, Some("round_robin")),
        first_two
    );
    assert_eq!(
        ModelDistributor::new().distribute("category:one", &entries, Some("round_robin")),
        first_two
    );
    assert_eq!(
        distributor.distribute("category:one", &entries, Some("round_robin")),
        first_two
    );
    let empty: Vec<Value> = Vec::new();
    assert_eq!(
        distributor.distribute("empty", &empty, Some("round_robin")),
        empty
    );
    assert_eq!(
        distributor.distribute("single", &[json!("provider/a")], Some("round_robin")),
        vec![json!("provider/a")]
    );
}

#[test]
fn parenthetical_and_variant_spellings_of_one_model_rotate_once() {
    let distributor = ModelDistributor::new();
    let entries = vec![
        json!("openai/gpt-6 (fast)"),
        json!("openai/gpt-6 high"),
        json!("openai/gpt-6"),
        json!("anthropic/claude-opus-5-5"),
    ];
    let unique = vec![
        json!("openai/gpt-6 (fast)"),
        json!("anthropic/claude-opus-5-5"),
    ];
    assert_eq!(
        distributor.distribute("category:deep", &entries, Some("round_robin")),
        unique
    );
    assert_eq!(
        distributor.distribute("category:deep", &entries, Some("round_robin")),
        vec![unique[1].clone(), unique[0].clone()]
    );
}
