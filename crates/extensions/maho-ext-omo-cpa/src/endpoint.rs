//! Port of omo-cpa `src/endpoint.ts`. Fetch the four CPA list formats and merge them around the
//! authoritative OpenAI ids.

use crate::provider_core::ANTHROPIC_BUG_PREFIX;
use crate::tier_types::{CatalogModel, Endpoint, ModalitySource};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::time::Duration;

pub const ANTHROPIC_VERSION: &str = "2023-06-01";
pub const MAX_SANE_OUTPUT_TOKENS: u64 = 250_000;
pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;

/// The three wire formats plus the Codex list variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CatalogListEndpoint {
    Openai,
    Anthropic,
    Gemini,
    Codex,
}

impl CatalogListEndpoint {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::Codex => "codex",
        }
    }

    const fn list_key(self) -> &'static str {
        match self {
            Self::Openai | Self::Anthropic => "data",
            Self::Gemini | Self::Codex => "models",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogIssueKind {
    DiscardedMetadata,
    MetadataDisagreement,
    MaxTokensClamped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogLimitField {
    ContextLength,
    MaxTokens,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogIssue {
    pub kind: CatalogIssueKind,
    pub endpoint: CatalogListEndpoint,
    pub model_id: String,
    pub field: Option<CatalogLimitField>,
    pub values: Option<Vec<u64>>,
    pub chosen: Option<u64>,
    pub from: Option<u64>,
    pub to: Option<u64>,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CatalogMergeResult {
    pub models: Vec<CatalogModel>,
    pub issues: Vec<CatalogIssue>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointSelection {
    pub endpoint: Endpoint,
    pub base_url_suffix: &'static str,
    pub headers: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CatalogFetchOk {
    pub models: Vec<CatalogModel>,
    pub issues: Vec<CatalogIssue>,
    pub failures: BTreeMap<CatalogListEndpoint, String>,
    /// Ids carried over from an earlier fetch because this round did not list them.
    pub retained_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CatalogFetchResult {
    Ok(CatalogFetchOk),
    Err {
        reason: String,
        failures: BTreeMap<CatalogListEndpoint, String>,
    },
}

/// OpenAI already exposes the canonical catalog id.
pub fn normalize_openai_id(id: &str) -> String {
    id.to_owned()
}

/// Normalize a model name from Gemini's `models/<id>` list representation.
pub fn normalize_gemini_id(id: &str) -> String {
    id.strip_prefix("models/").unwrap_or(id).to_owned()
}

/// Undo CPA's Anthropic-list id corruption. Unmangled ids are returned unchanged, because CPA may
/// fix the server bug independently.
///
/// Distinct from [`crate::provider_core::unmangle_anthropic_id`] on one edge: an empty remainder
/// yields the empty id here (`"".split("/")` joins to `""`), while the unmangler reports the bare
/// prefix. Both behaviors are load-bearing at their respective call sites.
pub fn normalize_anthropic_id(id: &str) -> String {
    let Some(rest) = id.strip_prefix(ANTHROPIC_BUG_PREFIX) else {
        return id.to_owned();
    };
    rest.split('/')
        .map(|segment| segment.chars().rev().collect::<String>())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("/")
}

/// Merge all available list representations around the authoritative OpenAI ids.
pub fn merge_catalogs(
    openai_response: &Value,
    anthropic_response: Option<&Value>,
    gemini_response: Option<&Value>,
    codex_response: Option<&Value>,
) -> CatalogMergeResult {
    let mut issues: Vec<CatalogIssue> = Vec::new();
    let mut models: Vec<CatalogModel> = Vec::new();
    let mut index: BTreeMap<String, usize> = BTreeMap::new();

    for record in records_at(openai_response, "data") {
        let Some(raw_id) = string_at(record, "id") else {
            continue;
        };
        let id = normalize_openai_id(&raw_id);
        if index.contains_key(&id) {
            continue;
        }
        let mut model = CatalogModel::new(id.clone());
        model.owned_by = string_at(record, "owned_by");
        index.insert(id.clone(), models.len());
        models.push(model);

        let position = models.len() - 1;
        models[position].context_length =
            read_record_context_length(&id, CatalogListEndpoint::Openai, record, &mut issues);
        models[position].max_tokens = choose_safer_limit(
            &id,
            CatalogLimitField::MaxTokens,
            &[
                number_at(record, "max_tokens"),
                number_at(record, "max_completion_tokens"),
            ],
            &mut issues,
            CatalogListEndpoint::Openai,
        );
    }

    if let Some(response) = anthropic_response {
        for record in records_at(response, "data") {
            let Some(raw_id) = string_at(record, "id") else {
                continue;
            };
            let id = normalize_anthropic_id(&raw_id);
            let Some(position) = index.get(&id).copied() else {
                issues.push(CatalogIssue {
                    kind: CatalogIssueKind::DiscardedMetadata,
                    endpoint: CatalogListEndpoint::Anthropic,
                    model_id: id.clone(),
                    field: None,
                    values: None,
                    chosen: None,
                    from: None,
                    to: None,
                    reason: format!(
                        "Anthropic metadata discarded because recovered id {id} is absent from the OpenAI catalog"
                    ),
                });
                continue;
            };

            if let Some(name) = string_at(record, "display_name") {
                models[position].display_name = Some(name);
            }
            if let Some(context) =
                read_record_context_length(&id, CatalogListEndpoint::Anthropic, record, &mut issues)
            {
                models[position].context_length = Some(context);
            }
            models[position].max_tokens = choose_safer_limit(
                &id,
                CatalogLimitField::MaxTokens,
                &[
                    number_at(record, "max_tokens"),
                    number_at(record, "max_completion_tokens"),
                ],
                &mut issues,
                CatalogListEndpoint::Anthropic,
            );
            models[position].thinking = boolean_at(record, "thinking")
                .or_else(|| boolean_at(record, "extended_thinking"));
            clamp_output_limit(&mut models[position], &mut issues, CatalogListEndpoint::Anthropic);
        }
    }

    if let Some(response) = gemini_response {
        for record in records_at(response, "models") {
            let Some(raw_id) = string_at(record, "name") else {
                continue;
            };
            let id = normalize_gemini_id(&raw_id);
            let Some(position) = index.get(&id).copied() else {
                issues.push(CatalogIssue {
                    kind: CatalogIssueKind::DiscardedMetadata,
                    endpoint: CatalogListEndpoint::Gemini,
                    model_id: id.clone(),
                    field: None,
                    values: None,
                    chosen: None,
                    from: None,
                    to: None,
                    reason: format!(
                        "Gemini metadata discarded because id {id} is absent from the OpenAI catalog"
                    ),
                });
                continue;
            };

            if models[position].display_name.is_none() {
                models[position].display_name = string_at(record, "displayName");
            }
            models[position].input_modalities = string_array_at(record, "supportedInputModalities");
            models[position].output_modalities =
                string_array_at(record, "supportedOutputModalities");
            if models[position].input_modalities.is_some() {
                models[position].input_modalities_source = Some(ModalitySource::Gemini);
            }
        }
    }

    // Codex exposes the maximum window under `models[].slug`, not `data[].id`, and it is the only
    // list that declares INPUT modalities for the models the Gemini list omits.
    if let Some(response) = codex_response {
        for record in records_at(response, "models") {
            let Some(id) = string_at(record, "slug") else {
                continue;
            };
            let Some(position) = index.get(&id).copied() else {
                continue;
            };

            models[position].context_length =
                read_record_context_length(&id, CatalogListEndpoint::Codex, record, &mut issues)
                    .or(models[position].context_length);
            models[position].max_tokens =
                number_at(record, "max_tokens").or(models[position].max_tokens);
            let codex_input = string_array_at(record, "input_modalities");
            if models[position].input_modalities.is_none()
                && let Some(input) = codex_input
            {
                models[position].input_modalities = Some(input);
                models[position].input_modalities_source = Some(ModalitySource::Codex);
            }
            clamp_output_limit(&mut models[position], &mut issues, CatalogListEndpoint::Codex);
        }
    }

    CatalogMergeResult { models, issues }
}

/// Choose the wire protocol from model identity, preferring enriched display metadata.
pub fn select_endpoint(model: &CatalogModel) -> EndpointSelection {
    let identity = format!(
        "{} {}",
        model.id,
        model.display_name.as_deref().unwrap_or("")
    )
    .to_lowercase();
    if has_family(&identity, "claude") {
        let mut headers = BTreeMap::new();
        headers.insert("anthropic-version".to_owned(), ANTHROPIC_VERSION.to_owned());
        return EndpointSelection {
            endpoint: Endpoint::Anthropic,
            base_url_suffix: "",
            headers,
        };
    }
    if has_family(&identity, "gemini") {
        return EndpointSelection {
            endpoint: Endpoint::Gemini,
            base_url_suffix: "/v1beta",
            headers: BTreeMap::new(),
        };
    }
    EndpointSelection {
        endpoint: Endpoint::Openai,
        base_url_suffix: "/v1",
        headers: BTreeMap::new(),
    }
}

/// Fetch the four list formats concurrently; OpenAI is the required identity source.
pub async fn fetch_catalog(
    client: &reqwest::Client,
    root: &str,
    api_key: &str,
    timeout_ms: u64,
) -> CatalogFetchResult {
    let base = root.trim_end_matches('/');
    let timeout = Duration::from_millis(timeout_ms);

    // Bound before the join: a `&format!()` temporary inside `tokio::join!` is dropped before the
    // futures are polled.
    let openai_url = format!("{base}/v1/models");
    let anthropic_url = format!("{base}/v1/models");
    let gemini_url = format!("{base}/v1beta/models");
    let codex_url = format!("{base}/v1/models?client_version=cpa");
    let (openai, anthropic, gemini, codex) = tokio::join!(
        fetch_list(
            client,
            &openai_url,
            api_key,
            CatalogListEndpoint::Openai,
            timeout
        ),
        fetch_list(
            client,
            &anthropic_url,
            api_key,
            CatalogListEndpoint::Anthropic,
            timeout
        ),
        fetch_list(
            client,
            &gemini_url,
            api_key,
            CatalogListEndpoint::Gemini,
            timeout
        ),
        fetch_list(
            client,
            &codex_url,
            api_key,
            CatalogListEndpoint::Codex,
            timeout
        ),
    );

    let openai = match openai {
        Ok(body) => body,
        Err(reason) => {
            let mut failures = BTreeMap::new();
            failures.insert(CatalogListEndpoint::Openai, reason.clone());
            return CatalogFetchResult::Err { reason, failures };
        }
    };

    let mut failures = BTreeMap::new();
    let mut optional = |endpoint: CatalogListEndpoint, result: Result<Value, String>| match result {
        Ok(body) => Some(body),
        Err(reason) => {
            failures.insert(endpoint, reason);
            None
        }
    };
    let anthropic_body = optional(CatalogListEndpoint::Anthropic, anthropic);
    let gemini_body = optional(CatalogListEndpoint::Gemini, gemini);
    let codex_body = optional(CatalogListEndpoint::Codex, codex);

    let merged = merge_catalogs(
        &openai,
        anthropic_body.as_ref(),
        gemini_body.as_ref(),
        codex_body.as_ref(),
    );
    if merged.models.is_empty() {
        let reason = "openai model list contained no valid model ids".to_owned();
        let mut openai_failure = failures;
        openai_failure.insert(CatalogListEndpoint::Openai, reason.clone());
        return CatalogFetchResult::Err {
            reason,
            failures: openai_failure,
        };
    }

    CatalogFetchResult::Ok(CatalogFetchOk {
        models: merged.models,
        issues: merged.issues,
        failures,
        retained_ids: Vec::new(),
    })
}

/// `(^|[/_.\s-])claude(?=$|[/_.\s-])`.
fn has_family(identity: &str, family: &str) -> bool {
    let bytes = identity.as_bytes();
    let family_bytes = family.as_bytes();
    let mut index = 0;
    while index + family_bytes.len() <= bytes.len() {
        if &bytes[index..index + family_bytes.len()] == family_bytes {
            let before = index == 0 || is_identity_separator(bytes[index - 1]);
            let after_index = index + family_bytes.len();
            let after = after_index == bytes.len() || is_identity_separator(bytes[after_index]);
            if before && after {
                return true;
            }
        }
        index += 1;
    }
    false
}

fn is_identity_separator(byte: u8) -> bool {
    matches!(byte, b'/' | b'_' | b'.' | b'-') || byte.is_ascii_whitespace()
}

/// Read a model's context window from one list record. An explicit `max_context_window` is the
/// provider's authoritative ceiling and wins over any coexisting `context_window`,
/// `context_length`, or `max_input_tokens`.
fn read_record_context_length(
    model_id: &str,
    endpoint: CatalogListEndpoint,
    record: &Map<String, Value>,
    issues: &mut Vec<CatalogIssue>,
) -> Option<u64> {
    if let Some(explicit_maximum) = number_at(record, "max_context_window") {
        return Some(explicit_maximum);
    }
    choose_safer_limit(
        model_id,
        CatalogLimitField::ContextLength,
        &[
            number_at(record, "context_window"),
            number_at(record, "context_length"),
            number_at(record, "max_input_tokens"),
        ],
        issues,
        endpoint,
    )
}

fn choose_safer_limit(
    model_id: &str,
    field: CatalogLimitField,
    candidates: &[Option<u64>],
    issues: &mut Vec<CatalogIssue>,
    endpoint: CatalogListEndpoint,
) -> Option<u64> {
    let mut values: Vec<u64> = Vec::new();
    for candidate in candidates.iter().flatten() {
        if !values.contains(candidate) {
            values.push(*candidate);
        }
    }
    let chosen = values.iter().copied().min()?;
    if values.len() > 1 {
        let joined = values
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let name = field_name(field);
        issues.push(CatalogIssue {
            kind: CatalogIssueKind::MetadataDisagreement,
            endpoint,
            model_id: model_id.to_owned(),
            field: Some(field),
            values: Some(values),
            chosen: Some(chosen),
            from: None,
            to: None,
            reason: format!("{name} candidates disagree ({joined}); chose safer value {chosen}"),
        });
    }
    Some(chosen)
}

fn clamp_output_limit(
    model: &mut CatalogModel,
    issues: &mut Vec<CatalogIssue>,
    endpoint: CatalogListEndpoint,
) {
    let (Some(max_tokens), Some(context_length)) = (model.max_tokens, model.context_length) else {
        return;
    };
    if max_tokens < context_length {
        return;
    }

    let below_context = context_length.saturating_sub(1);
    let to = if below_context > 0 {
        Some(MAX_SANE_OUTPUT_TOKENS.min(below_context))
    } else {
        None
    };
    model.max_tokens = to.filter(|to| *to < context_length);
    issues.push(CatalogIssue {
        kind: CatalogIssueKind::MaxTokensClamped,
        endpoint,
        model_id: model.id.clone(),
        field: Some(CatalogLimitField::MaxTokens),
        values: None,
        chosen: None,
        from: Some(max_tokens),
        to: model.max_tokens,
        reason: format!(
            "maxTokens {max_tokens} was not below contextLength {context_length}; clamped to {}",
            model
                .max_tokens
                .map_or_else(|| "null".to_owned(), |value| value.to_string())
        ),
    });
}

fn field_name(field: CatalogLimitField) -> &'static str {
    match field {
        CatalogLimitField::ContextLength => "contextLength",
        CatalogLimitField::MaxTokens => "maxTokens",
    }
}

async fn fetch_list(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    endpoint: CatalogListEndpoint,
    timeout: Duration,
) -> Result<Value, String> {
    let mut request = client
        .get(url)
        .header("Authorization", format!("Bearer {api_key}"))
        .timeout(timeout);
    if endpoint == CatalogListEndpoint::Anthropic {
        request = request.header("anthropic-version", ANTHROPIC_VERSION);
    }

    let response = request.send().await.map_err(|error| {
        format!("{} model list failed ({error})", endpoint.as_str())
    })?;
    if !response.status().is_success() {
        return Err(format!(
            "{} model list failed (HTTP {})",
            endpoint.as_str(),
            response.status().as_u16()
        ));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|error| {
            format!(
                "{} model list returned invalid JSON ({error})",
                endpoint.as_str()
            )
        })?;
    let list_key = endpoint.list_key();
    if records_at(&body, list_key).is_empty() {
        return Err(format!(
            "{} model list response has no {list_key} records",
            endpoint.as_str()
        ));
    }
    Ok(body)
}

fn records_at<'a>(value: &'a Value, key: &str) -> Vec<&'a Map<String, Value>> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|records| {
            records
                .iter()
                .filter_map(Value::as_object)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn string_at(record: &Map<String, Value>, key: &str) -> Option<String> {
    record
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn number_at(record: &Map<String, Value>, key: &str) -> Option<u64> {
    record
        .get(key)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value > 0.0)
        .map(|value| value as u64)
}

fn boolean_at(record: &Map<String, Value>, key: &str) -> Option<bool> {
    record.get(key).and_then(Value::as_bool)
}

fn string_array_at(record: &Map<String, Value>, key: &str) -> Option<Vec<String>> {
    let values = record.get(key)?.as_array()?;
    let strings: Vec<String> = values
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    (strings.len() == values.len()).then_some(strings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn openai_ids_are_the_identity_source() {
        let merged = merge_catalogs(
            &json!({"data": [{"id": "gpt-5.5", "owned_by": "openai"}]}),
            None,
            None,
            None,
        );
        assert_eq!(merged.models.len(), 1);
        assert_eq!(merged.models[0].id, "gpt-5.5");
        assert_eq!(merged.models[0].owned_by.as_deref(), Some("openai"));
        assert!(merged.issues.is_empty());
    }

    #[test]
    fn anthropic_metadata_attaches_only_to_recovered_ids() {
        let merged = merge_catalogs(
            &json!({"data": [{"id": "aion-labs/aion-2.0"}]}),
            Some(&json!({"data": [
                {"id": "claude-fable-5-dd-0.2-noia/sbal-noia", "display_name": "Aion 2.0", "thinking": true},
                {"id": "claude-fable-5-dd-unknown", "display_name": "Nope"}
            ]})),
            None,
            None,
        );
        assert_eq!(merged.models[0].display_name.as_deref(), Some("Aion 2.0"));
        assert_eq!(merged.models[0].thinking, Some(true));
        assert_eq!(merged.issues.len(), 1);
        assert_eq!(
            merged.issues[0].kind,
            CatalogIssueKind::DiscardedMetadata
        );
        assert_eq!(merged.issues[0].endpoint, CatalogListEndpoint::Anthropic);
    }

    #[test]
    fn gemini_supplies_modalities_and_wins_over_codex() {
        let merged = merge_catalogs(
            &json!({"data": [{"id": "gemini-3.1-pro"}, {"id": "grok-4.7"}]}),
            None,
            Some(&json!({"models": [
                {"name": "models/gemini-3.1-pro", "displayName": "Gemini 3.1 Pro", "supportedInputModalities": ["TEXT", "IMAGE", "VIDEO"], "supportedOutputModalities": ["TEXT"]}
            ]})),
            Some(&json!({"models": [
                {"slug": "gemini-3.1-pro", "input_modalities": ["text"]},
                {"slug": "grok-4.7", "input_modalities": ["text", "image"], "max_tokens": 8192}
            ]})),
        );
        let gemini = &merged.models[0];
        assert_eq!(gemini.display_name.as_deref(), Some("Gemini 3.1 Pro"));
        assert_eq!(
            gemini.input_modalities_source,
            Some(ModalitySource::Gemini)
        );
        assert_eq!(gemini.input_modalities.as_ref().map(Vec::len), Some(3));
        let grok = &merged.models[1];
        assert_eq!(grok.input_modalities_source, Some(ModalitySource::Codex));
        assert_eq!(grok.max_tokens, Some(8192));
    }

    #[test]
    fn disagreeing_limits_choose_the_safer_value_and_report_it() {
        let merged = merge_catalogs(
            &json!({"data": [{"id": "m", "context_window": 200000, "context_length": 8192}]}),
            None,
            None,
            None,
        );
        assert_eq!(merged.models[0].context_length, Some(8192));
        let issue = &merged.issues[0];
        assert_eq!(issue.kind, CatalogIssueKind::MetadataDisagreement);
        assert_eq!(issue.field, Some(CatalogLimitField::ContextLength));
        assert_eq!(issue.chosen, Some(8192));
    }

    #[test]
    fn an_explicit_max_context_window_beats_the_candidates() {
        let merged = merge_catalogs(
            &json!({"data": [{"id": "m", "max_context_window": 1_000_000, "context_window": 8192}]}),
            None,
            None,
            None,
        );
        assert_eq!(merged.models[0].context_length, Some(1_000_000));
        assert!(merged.issues.is_empty());
    }

    #[test]
    fn output_limits_are_clamped_below_the_context_window() {
        let merged = merge_catalogs(
            &json!({"data": [{"id": "m"}]}),
            Some(&json!({"data": [{"id": "m", "max_context_window": 1000, "max_tokens": 5000}]})),
            None,
            None,
        );
        assert_eq!(merged.models[0].max_tokens, Some(999));
        let issue = merged
            .issues
            .iter()
            .find(|issue| issue.kind == CatalogIssueKind::MaxTokensClamped)
            .expect("clamp issue");
        assert_eq!(issue.from, Some(5000));
        assert_eq!(issue.to, Some(999));
    }

    #[test]
    fn the_sane_output_ceiling_caps_a_generous_clamp() {
        let merged = merge_catalogs(
            &json!({"data": [{"id": "m"}]}),
            Some(&json!({"data": [{"id": "m", "max_context_window": 1_000_000, "max_tokens": 1_000_000}]})),
            None,
            None,
        );
        assert_eq!(merged.models[0].max_tokens, Some(MAX_SANE_OUTPUT_TOKENS));
    }

    fn selection(id: &str, name: Option<&str>) -> EndpointSelection {
        let mut model = CatalogModel::new(id);
        model.display_name = name.map(str::to_owned);
        select_endpoint(&model)
    }

    #[test]
    fn endpoint_selection_prefers_family_segments_over_substrings() {
        let claude = selection("claude-opus-5", None);
        assert_eq!(claude.endpoint, Endpoint::Anthropic);
        assert_eq!(claude.base_url_suffix, "");
        assert_eq!(
            claude.headers.get("anthropic-version").map(String::as_str),
            Some(ANTHROPIC_VERSION)
        );

        let gemini = selection("google/gemini-3.1-pro", None);
        assert_eq!(gemini.endpoint, Endpoint::Gemini);
        assert_eq!(gemini.base_url_suffix, "/v1beta");

        let opaque = selection("fable", Some("CLAUDE Fable 5"));
        assert_eq!(opaque.endpoint, Endpoint::Anthropic);

        let plain = selection("not-claudelike", Some("mygpt"));
        assert_eq!(plain.endpoint, Endpoint::Openai);
        assert_eq!(plain.base_url_suffix, "/v1");
    }

    #[test]
    fn gemini_ids_drop_their_list_prefix() {
        assert_eq!(normalize_gemini_id("models/gemini-3"), "gemini-3");
        assert_eq!(normalize_gemini_id("gemini-3"), "gemini-3");
    }

    #[test]
    fn anthropic_normalization_matches_the_ts_edge_cases() {
        assert_eq!(
            normalize_anthropic_id("claude-fable-5-dd-0.2-noia/sbal-noia"),
            "aion-labs/aion-2.0"
        );
        assert_eq!(normalize_anthropic_id("claude-opus-5"), "claude-opus-5");
        assert_eq!(normalize_anthropic_id(ANTHROPIC_BUG_PREFIX), "");
    }
}
