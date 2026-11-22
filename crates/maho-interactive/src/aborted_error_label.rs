//! Port of aborted-error-label.ts.
#[derive(Debug, Clone, Copy)]
pub enum AgentAbortSource {
    User,
    System,
    Provider,
}
pub fn aborted_error_label(
    persisted: Option<&str>,
    attempt: usize,
    source: Option<AgentAbortSource>,
) -> String {
    match source {
        Some(AgentAbortSource::User) => return "Operation aborted".into(),
        Some(AgentAbortSource::System) => return "System operation aborted".into(),
        Some(AgentAbortSource::Provider) => {
            return persisted.unwrap_or("Provider request failed").into();
        }
        None => {}
    }
    if let Some(p) = persisted {
        static PROVIDER_RETRY: std::sync::LazyLock<Option<regex::Regex>> =
            std::sync::LazyLock::new(|| {
                regex::Regex::new(r"^Provider retry failed after \d+ attempts?$").ok()
            });
        let provider_retry = PROVIDER_RETRY.as_ref().is_some_and(|r| r.is_match(p));
        if matches!(
            p,
            "Operation aborted" | "System operation aborted" | "Provider request failed"
        ) || p.starts_with("Provider request failed:")
            || provider_retry
        {
            return p.into();
        }
        if let Some(n) = p
            .strip_prefix("Aborted after ")
            .and_then(|v| {
                v.strip_suffix(" retry attempts")
                    .or_else(|| v.strip_suffix(" retry attempt"))
            })
            .filter(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()))
        {
            return format!(
                "Provider retry failed after {n} attempt{}",
                if n == "1" { "" } else { "s" }
            );
        }
    }
    if attempt > 0 {
        return format!(
            "Provider retry failed after {attempt} attempt{}",
            if attempt == 1 { "" } else { "s" }
        );
    }
    match persisted {
        Some(p) if p != "Request was aborted" => format!("Provider request failed: {p}"),
        _ => "Provider request failed".into(),
    }
}
/// Result text for a tool call the model emitted but that never ran because the turn ended (senpi#5064).
pub const UNRUN_AFTER_ERROR: &str = "Not run: the request failed.";

/// `rendered` has been through [`aborted_message_for_rendering`]. An abort keeps its label: when the
/// turn ends in a tool call, the card is the abort's only notice. An error stop shows its classified
/// notice once elsewhere, so the card never repeats the raw provider error.
pub fn unrun_tool_call_text(rendered: &serde_json::Value) -> String {
    let aborted = rendered
        .get("stopReason")
        .and_then(serde_json::Value::as_str)
        == Some("aborted");
    if !aborted {
        return UNRUN_AFTER_ERROR.to_owned();
    }
    rendered
        .get("errorMessage")
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.is_empty())
        .unwrap_or("Provider request failed")
        .to_owned()
}

pub fn aborted_message_for_rendering(
    message: &serde_json::Value,
    attempt: usize,
    source: Option<AgentAbortSource>,
) -> serde_json::Value {
    let mut rendered = message.clone();
    if message
        .get("stopReason")
        .and_then(serde_json::Value::as_str)
        == Some("aborted")
    {
        rendered["errorMessage"] = serde_json::Value::String(aborted_error_label(
            message
                .get("errorMessage")
                .and_then(serde_json::Value::as_str),
            attempt,
            source,
        ));
    }
    rendered
}
