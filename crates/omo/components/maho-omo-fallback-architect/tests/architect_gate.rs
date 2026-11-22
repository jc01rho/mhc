use maho_ext_api::*;
use maho_omo_fallback_architect::architect_gate::has_active_architect_category_with_env;
struct Registry(Vec<Model>);
impl ModelRegistry for Registry {
    fn get_all(&self) -> Vec<Model> {
        self.0.clone()
    }
    fn get_available(&self) -> Vec<Model> {
        self.0.clone()
    }
    fn find(&self, provider: &str, id: &str) -> Option<Model> {
        self.0
            .iter()
            .find(|m| m.provider == provider && m.id == id)
            .cloned()
    }
    fn has_configured_auth(&self, _: &Model) -> bool {
        true
    }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}
#[test]
fn live_registry_gate_and_explicit_disable() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let project = root.path().join("project");
    std::fs::create_dir(&home)?;
    std::fs::create_dir_all(project.join(".omo"))?;
    let env = std::collections::BTreeMap::from([
        ("HOME".into(), home.to_string_lossy().into_owned()),
        ("USERPROFILE".into(), home.to_string_lossy().into_owned()),
    ]);
    let make_model = |id: &str| -> Result<Model, serde_json::Error> {
        serde_json::from_value(
            serde_json::json!({"id":id,"name":"Fable","api":"anthropic-messages","provider":"anthropic","baseUrl":"http://localhost","reasoning":true,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":1000,"maxTokens":100}),
        )
    };
    let registry = Registry(vec![
        make_model("claude-fable-5")?,
        make_model("claude-fable-5-1")?,
        make_model("claude-fable-5-2")?,
        make_model("claude-opus-5-5")?,
    ]);
    // The builtin architect gate is Fable-only, so the execution gate (on by default) keeps it shut.
    assert!(!has_active_architect_category_with_env(
        &project,
        Some(env.clone()),
        Some(&registry)
    ));
    std::fs::write(
        project.join(".omo/omo.json"),
        r#"{"model_policy":{"block_fable":false}}"#,
    )?;
    assert!(has_active_architect_category_with_env(
        &project,
        Some(env.clone()),
        Some(&registry)
    ));
    assert!(!has_active_architect_category_with_env(
        &project,
        Some(env.clone()),
        Some(&Registry(vec![]))
    ));
    std::fs::write(
        project.join(".omo/omo.json"),
        r#"{"model_policy":{"block_fable":false},"categories":{"quick":{"model":"some/model"}}}"#,
    )?;
    assert!(has_active_architect_category_with_env(
        &project,
        Some(env.clone()),
        Some(&registry)
    ));
    let mut unrelated = registry.0[0].clone();
    unrelated.provider = "omo-mock".into();
    unrelated.id = "mock-weak".into();
    assert!(!has_active_architect_category_with_env(
        &project,
        Some(env.clone()),
        Some(&Registry(vec![unrelated]))
    ));
    std::fs::write(
        project.join(".omo/omo.json"),
        r#"{"model_policy":{"block_fable":false},"categories":{"architect":{"disable":true}}}"#,
    )?;
    assert!(!has_active_architect_category_with_env(
        &project,
        Some(env),
        Some(&registry)
    ));
    Ok(())
}
#[test]
fn isolated_config_gate_matrix() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let project = root.path().join("project");
    std::fs::create_dir(&home)?;
    std::fs::create_dir_all(project.join(".omo"))?;
    let env = std::collections::BTreeMap::from([
        ("HOME".into(), home.to_string_lossy().into_owned()),
        ("USERPROFILE".into(), home.to_string_lossy().into_owned()),
    ]);
    assert!(!has_active_architect_category_with_env(
        &project,
        Some(env.clone()),
        None
    ));
    for (config, expected) in [
        (
            r#"{"categories":{"architect":{"model":"anthropic/claude-fable-5"}}}"#,
            true,
        ),
        (
            r#"{"categories":{"architect":{"model":"anthropic/claude-fable-5","disable":true}}}"#,
            false,
        ),
        (r#"{"categories":{"quick":{"model":"some/model"}}}"#, false),
    ] {
        std::fs::write(project.join(".omo/omo.json"), config)?;
        assert_eq!(
            has_active_architect_category_with_env(&project, Some(env.clone()), None),
            expected
        );
    }
    Ok(())
}
