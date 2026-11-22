//! Port of `prompt-preset/active-preset.ts` (senpi#5665 / senpi#5686).

use crate::{
    contract::preset_shared_sections,
    index::{has_user_system_prompt, resolve_home},
    presets::{ModelMetadata, resolve_preset_name},
    settings::{PromptPresetName, load_prompt_preset_settings},
};
use maho_core::{dynamic_prompt::SharedSections, settings_manager::SettingsManager};
use maho_ext_api::{ExtensionContext, Model};

/// The preset this extension renders for `model` (default: the context's model), or `None` when none
/// applies: no model, or a user system prompt (`--system-prompt` / SDK loader override) outranks presets.
pub fn resolve_active_preset_name(
    ctx: &ExtensionContext,
    model: Option<&Model>,
) -> Option<PromptPresetName> {
    let model = model.or(ctx.model.as_ref())?;
    if has_user_system_prompt(&ctx.get_system_prompt_options()) {
        return None;
    }
    let home = resolve_home();
    let settings = SettingsManager::create(
        &ctx.cwd.to_string_lossy(),
        &ctx.agent_dir.to_string_lossy(),
        &home,
        (ctx.is_project_trusted_fn)(),
    );
    let setting = load_prompt_preset_settings(
        settings
            .get_project()
            .get("promptPreset")
            .and_then(|value| value.as_str()),
        settings
            .get_global()
            .get("promptPreset")
            .and_then(|value| value.as_str()),
    );
    let prompt_preset = ctx.model_registry.prompt_preset(model);
    resolve_preset_name(
        ModelMetadata {
            id: &model.id,
            provider: model.provider.as_str(),
            name: Some(&model.name),
            prompt_preset: prompt_preset.as_deref(),
        },
        setting,
    )
}

/// The shared-section wording of the active preset (senpi#5665), for extensions that append their own
/// shared prompt text after the preset renders.
pub fn active_shared_sections(ctx: &ExtensionContext) -> SharedSections {
    preset_shared_sections(resolve_active_preset_name(ctx, None))
}
