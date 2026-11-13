//! Port of omo-cpa `src/tier.ts`. Model tier classification plus the persistent override store
//! and the `/cpa tier` parser.
//!
//! The user's rule: muse / gpt / claude / gemini / glm / deepseek / grok are the real workhorses.
//! Everything else is a free, low-cost or low-capability pool reachable only as a last resort.
//! Nothing here is hardcoded to a model list: every classifying function is a pure function of the
//! catalog it is handed.

use crate::tier_types::{CatalogModel, PRIMARY_FAMILIES, Tier, TierDecision};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Prefix of an override key that applies to a whole family, e.g. `family:gpt`.
pub const FAMILY_OVERRIDE_PREFIX: &str = "family:";

/// id -> forced tier. The pure classifier's only external input.
pub type OverrideMap = BTreeMap<String, Tier>;

/// Strips a provider-side namespace, byte-for-byte as senpi does it: the cut is the LAST "." or "/".
///
/// Note the load-bearing consequence: for `z-ai/glm-5.3-flash` the dot inside "5.3" is the last
/// separator, so the candidate is `3-flash` and the id-side match fails; that model is rescued by
/// its displayName instead. Deviating here would make this plugin disagree with the router.
fn without_namespace(model_id: &str) -> &str {
    match model_id.rfind(['.', '/']) {
        Some(cut) => &model_id[cut + 1..],
        None => model_id,
    }
}

/// Conservative family match on the id: exact id or a dash-suffixed variant, on either the raw id
/// or the namespace-stripped one. Never an arbitrary substring.
pub fn matches_family_by_id(model_id: &str, family: &str) -> bool {
    let lower = model_id.to_lowercase();
    let family = family.to_lowercase();
    [lower.as_str(), without_namespace(&lower)]
        .into_iter()
        .any(|candidate| candidate == family || candidate.starts_with(&format!("{family}-")))
}

/// First primary family whose id-side rule the model id satisfies, else `None`.
pub fn family_from_id(model_id: &str) -> Option<String> {
    for family in PRIMARY_FAMILIES {
        if matches_family_by_id(model_id, family) {
            return Some((*family).to_owned());
        }
    }
    // Provider-prefixed Claude/GPT ids retain their family identity. Match a complete segment,
    // never glued names such as "mygpt" or "claudelike".
    let lower = model_id.to_lowercase();
    let bytes = lower.as_bytes();
    for family in ["claude", "gpt"] {
        let needle = family.as_bytes();
        let mut index = 0;
        while index + needle.len() <= bytes.len() {
            if &bytes[index..index + needle.len()] == needle {
                let before = index == 0 || matches!(bytes[index - 1], b'/' | b'_' | b'.' | b'-');
                let after_index = index + needle.len();
                let after_is_dash = bytes.get(after_index) == Some(&b'-');
                let after_is_end = after_index == bytes.len();
                if before && (after_is_dash || after_is_end) {
                    return Some(family.to_owned());
                }
            }
            index += 1;
        }
    }
    None
}

/// Lowercased alphanumeric words. "Z.ai" -> ["z","ai"], "Dots3-Note" -> ["dots3","note"].
fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Family match on the displayName, which for a large part of the catalog is the only true identity
/// signal (`fable` is `claude-fable-5`).
///
/// The rule: the family must be the HEAD word of the name, or the head word after a vendor prefix
/// (`Z.ai: GLM 5.3 Flash`, `Meta: Muse Spark 1.3`). A family mentioned anywhere else is commentary.
pub fn family_from_display_name(display_name: Option<&str>) -> Option<String> {
    let display_name = display_name?;
    for segment in display_name.split(':') {
        let Some(head) = tokenize(segment).into_iter().next() else {
            continue;
        };
        for family in PRIMARY_FAMILIES {
            if head == *family {
                return Some((*family).to_owned());
            }
        }
    }
    None
}

/// Free / zero-cost marker, matched as a whole word only so `freeform-writer-7b` and
/// "Freedom Model 2" are untouched.
pub fn has_free_marker(model: &CatalogModel) -> bool {
    if tokenize(&model.id).iter().any(|token| token == "free") {
        return true;
    }
    model
        .display_name
        .as_deref()
        .is_some_and(|name| tokenize(name).iter().any(|token| token == "free"))
}

/// Words denoting a non-text generator or a classifier.
const NON_CHAT_MARKERS: &[&str] = &[
    // capability nouns
    "image",
    "images",
    "music",
    "audio",
    "video",
    "speech",
    "voice",
    "tts",
    "stt",
    "embed",
    "embedding",
    "embeddings",
    "rerank",
    "reranker",
    "moderation",
    "transcription",
    "transcribe",
    "diffusion",
    "upscale",
    "upscaler",
    // non-text generator / classifier families
    "lyria",
    "imagen",
    "veo",
    "sora",
    "whisper",
    "dalle",
    "guard",
];

/// Multi-word markers, matched on adjacent token pairs so one innocuous word cannot trigger them.
const NON_CHAT_BIGRAMS: &[&str] = &["content safety", "text embedding", "image generation"];

fn marker_words(model: &CatalogModel) -> Vec<String> {
    let mut words = tokenize(&model.id);
    if let Some(name) = &model.display_name {
        words.extend(tokenize(name));
    }
    words
}

fn non_chat_name_marker(model: &CatalogModel) -> Option<String> {
    let words = marker_words(model);
    if let Some(single) = words
        .iter()
        .find(|word| NON_CHAT_MARKERS.contains(&word.as_str()))
    {
        return Some(single.clone());
    }
    for pair in words.windows(2) {
        let joined = format!("{} {}", pair[0], pair[1]);
        if NON_CHAT_BIGRAMS.iter().any(|bigram| *bigram == joined) {
            return Some(joined);
        }
    }
    None
}

/// Whether a model can carry a chat turn.
///
/// Two independent signals, because neither alone is sufficient on the live catalog:
///  1. declared output modality without "text" -> not chat-capable
///  2. a non-text generator/classifier name   -> not chat-capable
///
/// Deliberately NOT signals: the word "preview", and INPUT modality (a vision model that replies
/// in text is a chat model).
pub fn is_chat_capable(model: &CatalogModel) -> bool {
    if non_chat_name_marker(model).is_some() {
        return false;
    }
    match &model.output_modalities {
        Some(modalities) if !modalities.is_empty() => modalities
            .iter()
            .any(|modality| modality.to_lowercase() == "text"),
        // Null modalities are the common case and must never be a blanket exclusion.
        _ => true,
    }
}

/// Why a model is unfit for chat, naming WHICH signal fired so the reason is actionable.
fn non_chat_reason(model: &CatalogModel) -> String {
    if let Some(named) = non_chat_name_marker(model) {
        return format!("non-text model family \"{named}\" in its name — unfit for a chat chain");
    }
    let declared = match &model.output_modalities {
        Some(modalities) if !modalities.is_empty() => modalities
            .iter()
            .map(|modality| modality.to_lowercase())
            .collect::<Vec<_>>()
            .join("/"),
        _ => "unknown".to_owned(),
    };
    format!("output modality {declared} has no text — unfit for a chat chain")
}

/// Classify a whole catalog. Pure, synchronous, order-preserving, no I/O.
pub fn classify(
    models: &[CatalogModel],
    overrides: &OverrideMap,
    upstream_by_alias: &BTreeMap<String, String>,
) -> Vec<TierDecision> {
    models
        .iter()
        .map(|model| {
            let upstream = upstream_by_alias
                .get(&model.id)
                .cloned()
                .or_else(|| model.upstream_model_id.clone());
            let override_tier = overrides
                .get(&model.id)
                .copied()
                .or_else(|| family_override(model, upstream.as_deref(), overrides));
            classify_one(model, override_tier, upstream.as_deref())
        })
        .collect()
}

/// A per-id override wins; otherwise a `family:<name>` override covers every model of that family.
fn family_override(
    model: &CatalogModel,
    upstream_id: Option<&str>,
    overrides: &OverrideMap,
) -> Option<Tier> {
    let id = upstream_id.unwrap_or(&model.id);
    let family = family_from_id(id).or_else(|| {
        family_from_display_name(if upstream_id.is_some() {
            upstream_id
        } else {
            model.display_name.as_deref()
        })
    })?;
    overrides
        .get(&format!("{FAMILY_OVERRIDE_PREFIX}{family}"))
        .copied()
}

fn decide(
    id: &str,
    tier: Tier,
    family: Option<String>,
    reason: String,
    overridden: bool,
) -> TierDecision {
    // Invariant: `family` is non-null only for a model actually usable as that family right now.
    TierDecision {
        id: id.to_owned(),
        tier,
        family: if tier == Tier::Primary { family } else { None },
        reason,
        overridden,
    }
}

/// Rule order, strongest signal first:
///   0. user override            -> wins outright
///   1. non-chat output modality -> last
///   2. free / zero-cost marker  -> last
///   3. family via id            -> primary
///   4. family via displayName   -> primary (the alias rescue)
///   5. otherwise                -> last
fn classify_one(
    model: &CatalogModel,
    override_tier: Option<Tier>,
    upstream_id: Option<&str>,
) -> TierDecision {
    if let Some(tier) = override_tier {
        let known = family_from_id(&model.id)
            .or_else(|| family_from_display_name(model.display_name.as_deref()));
        let verb = if tier == Tier::Primary {
            "promoted to primary"
        } else {
            "demoted to last-resort"
        };
        return decide(
            &model.id,
            tier,
            known,
            format!("user override: {verb}"),
            true,
        );
    }
    let identity = identity(model, upstream_id);
    if !is_chat_capable(&identity) {
        return decide(
            &model.id,
            Tier::Last,
            None,
            non_chat_reason(&identity),
            false,
        );
    }
    if has_free_marker(&identity) {
        return decide(
            &model.id,
            Tier::Last,
            None,
            "free / zero-cost marker".to_owned(),
            false,
        );
    }
    if let Some(family) = family_from_id(&identity.id) {
        let via = match upstream_id {
            Some(upstream) => format!("upstream {upstream}"),
            None => "id".to_owned(),
        };
        return decide(
            &model.id,
            Tier::Primary,
            Some(family.clone()),
            format!("family {family} via {via}"),
            false,
        );
    }
    if let Some(family) = family_from_display_name(identity.display_name.as_deref()) {
        let name = identity.display_name.as_deref().unwrap_or_default();
        return decide(
            &model.id,
            Tier::Primary,
            Some(family.clone()),
            format!("family {family} via displayName \"{name}\""),
            false,
        );
    }
    let reason = match upstream_id {
        Some(upstream) => format!("upstream {upstream} matches no primary family"),
        None => "no primary family matched".to_owned(),
    };
    decide(&model.id, Tier::Last, None, reason, false)
}

fn identity(model: &CatalogModel, upstream_id: Option<&str>) -> CatalogModel {
    match upstream_id {
        Some(upstream) => CatalogModel {
            id: upstream.to_owned(),
            display_name: Some(upstream.to_owned()),
            ..model.clone()
        },
        None => model.clone(),
    }
}

/// An override whose model id is not in the current catalog. Kept, not dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InactiveOverride {
    pub id: String,
    pub tier: Tier,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TierReport {
    /// Every input model, in catalog order. primary + last + chatUnfit.
    pub decisions: Vec<TierDecision>,
    pub primary: Vec<TierDecision>,
    /// Routable last-resort pool. This IS the chain tail.
    pub last: Vec<TierDecision>,
    /// Models that cannot hold a conversation at all (music, image, embedding, classifiers).
    pub chat_unfit: Vec<TierDecision>,
    pub inactive_overrides: Vec<InactiveOverride>,
}

/// Classify and split into the two pools, reporting overrides that currently match nothing.
pub fn build_tier_report(
    models: &[CatalogModel],
    overrides: &OverrideMap,
    upstream_by_alias: &BTreeMap<String, String>,
) -> TierReport {
    let decisions = classify(models, overrides, upstream_by_alias);
    let present: BTreeSet<&str> = models.iter().map(|model| model.id.as_str()).collect();
    let inactive_overrides = overrides
        .iter()
        .filter(|(id, _)| !id.starts_with(FAMILY_OVERRIDE_PREFIX) && !present.contains(id.as_str()))
        .map(|(id, tier)| InactiveOverride {
            id: id.clone(),
            tier: *tier,
            reason: "현재 카탈로그에 없는 모델 — 설정은 보존됨 (비활성)".to_owned(),
        })
        .collect();

    // An explicit user override keeps a model routable even when the automatic rule considers it
    // unfit: the user is the final authority on their routing.
    let by_id: BTreeMap<&str, &CatalogModel> = models
        .iter()
        .map(|model| (model.id.as_str(), model))
        .collect();
    let unfit = |decision: &TierDecision| {
        if decision.overridden {
            return false;
        }
        let Some(model) = by_id.get(decision.id.as_str()) else {
            return false;
        };
        let upstream = upstream_by_alias
            .get(&decision.id)
            .map(String::as_str)
            .or(model.upstream_model_id.as_deref());
        !is_chat_capable(&identity(model, upstream))
    };

    TierReport {
        primary: decisions
            .iter()
            .filter(|decision| decision.tier == Tier::Primary && !unfit(decision))
            .cloned()
            .collect(),
        last: decisions
            .iter()
            .filter(|decision| decision.tier == Tier::Last && !unfit(decision))
            .cloned()
            .collect(),
        chat_unfit: decisions
            .iter()
            .filter(|decision| unfit(decision))
            .cloned()
            .collect(),
        decisions,
        inactive_overrides,
    }
}

// ===========================================================================
// Persistent override store. Strictly separate from the pure classifier above.
// ===========================================================================

/// One recorded user decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverrideEntry {
    pub tier: Tier,
    /// Epoch ms, so the UI can show when a choice was made.
    pub set_at: i64,
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverrideStore {
    pub overrides: BTreeMap<String, OverrideEntry>,
}

impl Default for OverrideStore {
    fn default() -> Self {
        Self::empty()
    }
}

impl OverrideStore {
    pub fn empty() -> Self {
        Self {
            overrides: BTreeMap::new(),
        }
    }
}

/// The plugin's own cache dir, matching the TS convention.
pub fn overrides_file() -> PathBuf {
    cache_dir().join("tier-overrides.json")
}

pub fn cache_dir() -> PathBuf {
    super::management_key::home_dir()
        .join(".cache")
        .join("omo-cpa")
}

/// Project the store down to what the pure classifier consumes.
pub fn to_override_map(store: &OverrideStore) -> OverrideMap {
    store
        .overrides
        .iter()
        .map(|(id, entry)| (id.clone(), entry.tier))
        .collect()
}

/// Immutable insert/replace.
pub fn set_override(
    store: &OverrideStore,
    id: &str,
    tier: Tier,
    note: Option<&str>,
    now_ms: i64,
) -> OverrideStore {
    let mut next = store.clone();
    next.overrides.insert(
        id.to_owned(),
        OverrideEntry {
            tier,
            set_at: now_ms,
            note: note.map(str::to_owned),
        },
    );
    next
}

/// Immutable delete. Removing an absent id is a no-op, never an error.
pub fn clear_override(store: &OverrideStore, id: &str) -> OverrideStore {
    if !store.overrides.contains_key(id) {
        return store.clone();
    }
    let mut next = store.clone();
    next.overrides.remove(id);
    next
}

/// omo's own configuration tree is user-owned and read-only for this plugin. The guard covers the
/// entire `~/.omo` directory rather than a list of filenames.
pub fn is_forbidden_override_path(path: &Path) -> bool {
    let omo_root = super::management_key::home_dir().join(".omo");
    let canonical = |value: &Path| std::fs::canonicalize(value).unwrap_or_else(|_| absolute(value));
    let root = canonical(&omo_root);
    let target = canonical(path);
    target == root || target.starts_with(&root)
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

/// Defensive parse. Anything unrecognised is dropped entry-by-entry rather than failing the whole
/// file: one bad line written by a future version must not cost the user every preference.
fn parse_store(raw: &Value) -> OverrideStore {
    let Some(overrides) = raw.get("overrides").and_then(Value::as_object) else {
        return OverrideStore::empty();
    };
    let mut entries = BTreeMap::new();
    for (id, value) in overrides {
        let Some(record) = value.as_object() else {
            continue;
        };
        let tier = match record.get("tier").and_then(Value::as_str) {
            Some("primary") => Tier::Primary,
            Some("last") => Tier::Last,
            _ => continue,
        };
        entries.insert(
            id.clone(),
            OverrideEntry {
                tier,
                set_at: record
                    .get("setAt")
                    .and_then(Value::as_f64)
                    .map_or(0, |value| value as i64),
                note: record
                    .get("note")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            },
        );
    }
    OverrideStore { overrides: entries }
}

/// Load persisted overrides. A missing, unreadable, corrupt or structurally wrong file degrades to
/// "no overrides" and never fails: a broken preferences file must not take a session down.
pub fn load_override_store(path: &Path) -> OverrideStore {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .map(|value| parse_store(&value))
        .unwrap_or_default()
}

/// Persist overrides. Returns false instead of failing when it cannot write.
pub fn save_override_store(store: &OverrideStore, path: &Path) -> bool {
    if is_forbidden_override_path(path) {
        return false;
    }
    let mut overrides = Map::new();
    for (id, entry) in &store.overrides {
        let mut record = Map::new();
        record.insert(
            "tier".to_owned(),
            Value::String(entry.tier.as_str().to_owned()),
        );
        record.insert("setAt".to_owned(), Value::from(entry.set_at));
        if let Some(note) = &entry.note {
            record.insert("note".to_owned(), Value::String(note.clone()));
        }
        overrides.insert(id.clone(), Value::Object(record));
    }
    let mut root = Map::new();
    root.insert("version".to_owned(), Value::from(1));
    root.insert("overrides".to_owned(), Value::Object(overrides));
    let text = serde_json::to_string_pretty(&Value::Object(root)).unwrap_or_default();
    if let Some(parent) = path.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return false;
    }
    std::fs::write(path, format!("{text}\n")).is_ok()
}

// ===========================================================================
// /cpa subcommand parsing. Pure, so wiring it into the extension is one call.
// ===========================================================================

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TierCommand {
    List,
    Set { id: String, tier: Tier },
    Clear { id: String },
    Error { message: String },
}

const TIER_USAGE: &str = "사용법: /cpa tier [promote|demote|reset] <model-id>";

/// Parse the `tier` subcommand of /cpa. `None` when the args are not a tier command at all, so the
/// caller can fall through to its other subcommands.
pub fn parse_tier_command(args: &[String]) -> Option<TierCommand> {
    let head = args.first()?;
    if head != "tier" {
        return None;
    }
    let Some(action) = args.get(1) else {
        return Some(TierCommand::List);
    };

    if matches!(action.as_str(), "promote" | "demote" | "reset") {
        let Some(id) = args.get(2).filter(|id| !id.is_empty()) else {
            return Some(TierCommand::Error {
                message: format!("모델 id가 필요합니다. {TIER_USAGE}"),
            });
        };
        if action == "reset" {
            return Some(TierCommand::Clear { id: id.clone() });
        }
        return Some(TierCommand::Set {
            id: id.clone(),
            tier: if action == "promote" {
                Tier::Primary
            } else {
                Tier::Last
            },
        });
    }
    Some(TierCommand::Error {
        message: format!("알 수 없는 하위 명령 \"{action}\". {TIER_USAGE}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str) -> CatalogModel {
        CatalogModel::new(id)
    }

    fn named(id: &str, name: &str) -> CatalogModel {
        CatalogModel {
            display_name: Some(name.to_owned()),
            ..CatalogModel::new(id)
        }
    }

    fn classify_one_model(model: &CatalogModel) -> TierDecision {
        classify(
            std::slice::from_ref(model),
            &OverrideMap::new(),
            &BTreeMap::new(),
        )
        .remove(0)
    }

    #[test]
    fn family_matching_uses_the_last_namespace_separator() {
        assert!(matches_family_by_id("claude-opus-5", "claude"));
        assert!(matches_family_by_id(
            "global.anthropic.claude-opus-5",
            "claude"
        ));
        assert!(matches_family_by_id("claude", "claude"));
        assert!(!matches_family_by_id("not-claude-fable-5", "claude"));
        // The dot in "5.3" is the last separator, so the id-side match fails by design.
        assert!(!matches_family_by_id("z-ai/glm-5.3-flash", "glm"));
    }

    #[test]
    fn family_from_id_prefers_the_first_declared_family() {
        assert_eq!(family_from_id("gpt-5.5").as_deref(), Some("gpt"));
        assert_eq!(
            family_from_id("vendors/claude-opus-5").as_deref(),
            Some("claude")
        );
        assert_eq!(family_from_id("mygpt").as_deref(), None);
        assert_eq!(family_from_id("claudelike").as_deref(), None);
        assert_eq!(family_from_id("muse-spark-1.3").as_deref(), Some("muse"));
    }

    #[test]
    fn display_names_supply_the_family_only_at_the_head_word() {
        assert_eq!(
            family_from_display_name(Some("Z.ai: GLM 5.3 Flash")).as_deref(),
            Some("glm")
        );
        assert_eq!(
            family_from_display_name(Some("Meta: Muse Spark 1.3")).as_deref(),
            Some("muse")
        );
        assert_eq!(
            family_from_display_name(Some("Qwen 3 (claude-compatible API)")),
            None
        );
        assert_eq!(family_from_display_name(None), None);
    }

    #[test]
    fn free_markers_are_whole_words() {
        assert!(has_free_marker(&model("vendor/thing:free")));
        assert!(has_free_marker(&named("opaque", "Some Model (Free)")));
        assert!(!has_free_marker(&model("freeform-writer-7b")));
        assert!(!has_free_marker(&named("freedom", "Freedom Model 2")));
    }

    #[test]
    fn non_chat_models_are_excluded_by_modality_or_name() {
        let music = model("google/lyria-3-pro-preview");
        assert!(!is_chat_capable(&music));
        let image = CatalogModel {
            input_modalities: Some(vec!["TEXT".into(), "IMAGE".into()]),
            output_modalities: Some(vec!["IMAGE".into()]),
            ..model("dall-e-4")
        };
        assert!(!is_chat_capable(&image));
        let captioner = CatalogModel {
            output_modalities: Some(vec!["TEXT".into(), "IMAGE".into()]),
            ..model("gemini-3.1-flash-image")
        };
        assert!(!is_chat_capable(&captioner));
        let vision = CatalogModel {
            output_modalities: Some(vec!["TEXT".into()]),
            ..model("gemini-3-pro-preview")
        };
        assert!(is_chat_capable(&vision));
        assert!(is_chat_capable(&model("mystery-model")));
        let classifier = named("safety", "Content Safety Guard");
        assert!(!is_chat_capable(&classifier));
    }

    #[test]
    fn the_alias_rescue_lands_a_named_claude_in_primary() {
        let decision = classify_one_model(&named("fable", "Claude Fable 5"));
        assert_eq!(decision.tier, Tier::Primary);
        assert_eq!(decision.family.as_deref(), Some("claude"));
        assert!(decision.reason.contains("via displayName"));
    }

    #[test]
    fn free_and_non_chat_models_fall_to_last_without_a_family() {
        let free = classify_one_model(&model("vendor/free-model:free"));
        assert_eq!(free.tier, Tier::Last);
        assert!(free.reason.contains("free"));
        let music = classify_one_model(&model("google/lyria-3"));
        assert_eq!(music.tier, Tier::Last);
        assert_eq!(music.family, None);
        assert!(music.reason.contains("non-text"));
    }

    #[test]
    fn an_upstream_alias_is_judged_by_its_upstream_identity() {
        let mut model = named("opaque-alias", "Some Alias");
        model.upstream_model_id = Some("claude-opus-5".to_owned());
        let decision = classify_one_model(&model);
        assert_eq!(decision.tier, Tier::Primary);
        assert_eq!(decision.family.as_deref(), Some("claude"));
        assert!(decision.reason.contains("upstream claude-opus-5"));
    }

    #[test]
    fn a_override_wins_and_family_overrides_cover_the_whole_family() {
        let catalog = vec![
            model("claude-opus-5"),
            model("vendor/free-thing:free"),
            model("mystery"),
        ];
        let mut overrides = OverrideMap::new();
        overrides.insert("mystery".to_owned(), Tier::Primary);
        overrides.insert(format!("{FAMILY_OVERRIDE_PREFIX}claude"), Tier::Last);
        let decisions = classify(&catalog, &overrides, &BTreeMap::new());
        assert_eq!(decisions[0].tier, Tier::Last);
        assert!(decisions[0].overridden);
        assert_eq!(decisions[0].family, None);
        assert_eq!(decisions[2].tier, Tier::Primary);
        assert!(decisions[2].overridden);
    }

    #[test]
    fn the_report_splits_pools_and_keeps_inactive_overrides() {
        let catalog = vec![
            model("claude-opus-5"),
            model("vendor/free-thing:free"),
            model("google/lyria-3"),
        ];
        let mut overrides = OverrideMap::new();
        overrides.insert("absent-model".to_owned(), Tier::Primary);
        let report = build_tier_report(&catalog, &overrides, &BTreeMap::new());
        assert_eq!(report.primary.len(), 1);
        assert_eq!(report.last.len(), 1);
        assert_eq!(report.chat_unfit.len(), 1);
        assert_eq!(report.decisions.len(), 3);
        assert_eq!(report.inactive_overrides.len(), 1);
        assert_eq!(report.inactive_overrides[0].id, "absent-model");
    }

    #[test]
    fn an_override_keeps_a_non_chat_model_out_of_the_unfit_pool() {
        let catalog = vec![model("google/lyria-3")];
        let mut overrides = OverrideMap::new();
        overrides.insert("google/lyria-3".to_owned(), Tier::Primary);
        let report = build_tier_report(&catalog, &overrides, &BTreeMap::new());
        assert_eq!(report.primary.len(), 1);
        assert!(report.chat_unfit.is_empty());
    }

    #[test]
    fn the_override_store_round_trips_and_drops_bad_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tier-overrides.json");
        let store = set_override(
            &OverrideStore::empty(),
            "claude-opus-5",
            Tier::Last,
            Some("note"),
            7,
        );
        assert!(save_override_store(&store, &path));
        let loaded = load_override_store(&path);
        assert_eq!(loaded.overrides["claude-opus-5"].tier, Tier::Last);
        assert_eq!(loaded.overrides["claude-opus-5"].set_at, 7);
        assert_eq!(
            loaded.overrides["claude-opus-5"].note.as_deref(),
            Some("note")
        );

        std::fs::write(
            &path,
            r#"{"version":1,"overrides":{"good":{"tier":"primary","setAt":1},"bad":{"tier":"sideways"},"shape":3}}"#,
        )
        .expect("write");
        let loaded = load_override_store(&path);
        assert_eq!(loaded.overrides.len(), 1);
        assert!(loaded.overrides.contains_key("good"));
    }

    #[test]
    fn a_corrupt_store_file_reads_as_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tier-overrides.json");
        std::fs::write(&path, "{not json").expect("write");
        assert!(load_override_store(&path).overrides.is_empty());
        assert!(
            load_override_store(&dir.path().join("absent"))
                .overrides
                .is_empty()
        );
    }

    #[test]
    fn clearing_an_absent_override_is_a_no_op() {
        let store = set_override(&OverrideStore::empty(), "a", Tier::Primary, None, 1);
        assert_eq!(clear_override(&store, "b"), store);
        assert!(clear_override(&store, "a").overrides.is_empty());
    }

    #[test]
    fn the_omo_tree_is_forbidden_for_overrides() {
        let omo = super::super::management_key::home_dir().join(".omo");
        assert!(is_forbidden_override_path(&omo));
        assert!(is_forbidden_override_path(
            &omo.join("agent").join("x.json")
        ));
        assert!(!is_forbidden_override_path(
            &super::super::management_key::home_dir().join(".cache/omo-cpa/x.json")
        ));
    }

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn the_tier_command_parser_covers_every_documented_form() {
        assert_eq!(parse_tier_command(&words("tier")), Some(TierCommand::List));
        assert_eq!(
            parse_tier_command(&words("tier promote claude-opus-5")),
            Some(TierCommand::Set {
                id: "claude-opus-5".into(),
                tier: Tier::Primary
            })
        );
        assert_eq!(
            parse_tier_command(&words("tier demote claude-opus-5")),
            Some(TierCommand::Set {
                id: "claude-opus-5".into(),
                tier: Tier::Last
            })
        );
        assert_eq!(
            parse_tier_command(&words("tier reset claude-opus-5")),
            Some(TierCommand::Clear {
                id: "claude-opus-5".into()
            })
        );
        assert_eq!(parse_tier_command(&words("refresh")), None);
        assert!(matches!(
            parse_tier_command(&words("tier promote")),
            Some(TierCommand::Error { .. })
        ));
        assert!(matches!(
            parse_tier_command(&words("tier sideways")),
            Some(TierCommand::Error { .. })
        ));
    }
}
