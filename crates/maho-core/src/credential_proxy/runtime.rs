//! Port of senpi `packages/coding-agent/src/core/credential-proxy/runtime.ts` and the `credential-proxy` builtin
//! (`core/extensions/builtin/credential-proxy/index.ts`) (6d4426a28; senpi#6828).
//!
//! The holds live in `maho_tools::credential_proxy_holds` so every crate that builds a child environment reads
//! them; this module parses the declarations, publishes the hold and starts the proxy.
//!
//! mhc divergence: senpi starts the proxy from a builtin extension on every `session_start`. Here session
//! creation activates it once per engine process (holder [`ENGINE_HOLDER`]) and an identical declaration set
//! keeps the running proxy, so in-process sessions (subagents, reloads) never close a route a sibling's children
//! are still using.
use super::declarations::{CredentialDeclaration, parse_credential_declarations};
use super::proxy::{CredentialProxy, ProxiedCredential, start_credential_proxy};
use maho_tools::credential_proxy_holds::{
    CredentialHold, is_current, release_hold, set_hold, set_route,
};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, PoisonError};

pub use maho_tools::credential_proxy_holds::{CREDENTIAL_PLACEHOLDER, with_credential_proxy};

/// The holder session creation activates for the engine process.
pub const ENGINE_HOLDER: &str = "engine";

#[derive(Default)]
struct Activations {
    proxies: HashMap<String, CredentialProxy>,
    fingerprints: HashMap<String, String>,
}

static ACTIVATIONS: Mutex<Option<Activations>> = Mutex::new(None);

fn with_activations<T>(f: impl FnOnce(&mut Activations) -> T) -> T {
    let mut guard = ACTIVATIONS.lock().unwrap_or_else(PoisonError::into_inner);
    f(guard.get_or_insert_with(Activations::default))
}

fn detach(holder: &str) -> Option<CredentialProxy> {
    release_hold(holder);
    with_activations(|activations| {
        activations.fingerprints.remove(holder);
        activations.proxies.remove(holder)
    })
}

/// Withholds every declared credential at once, then starts the proxy. Returns after the proxy listens; fails
/// when it cannot, leaving the credentials withheld and unrouted.
pub async fn activate_credential_proxy(
    holder: &str,
    settings: Option<&Value>,
    env: &BTreeMap<String, String>,
    warn: &(dyn Fn(&str) + Sync),
) -> std::io::Result<()> {
    let parsed = parse_credential_declarations(settings);
    let values: BTreeMap<&String, Option<&String>> = parsed
        .withheld_names
        .iter()
        .map(|name| (name, env.get(name)))
        .collect();
    let fingerprint = serde_json::json!({ "settings": settings, "values": values }).to_string();
    let unchanged = with_activations(|activations| {
        activations.fingerprints.get(holder) == Some(&fingerprint)
            && activations
                .proxies
                .get(holder)
                .is_none_or(CredentialProxy::is_running)
    });
    if unchanged {
        return Ok(());
    }
    let previous = detach(holder);
    let generation = (!parsed.withheld_names.is_empty()).then(|| {
        set_hold(
            holder,
            CredentialHold {
                withheld: parsed
                    .withheld_names
                    .iter()
                    .map(|name| (name.clone(), env.get(name).cloned()))
                    .collect(),
                routes: Vec::new(),
            },
        )
    });
    if let Some(previous) = previous {
        previous.close();
    }
    for problem in &parsed.problems {
        warn(problem);
    }
    let Some(generation) = generation else {
        remember(holder, fingerprint);
        return Ok(());
    };
    let mut proxied = Vec::new();
    for declaration in &parsed.credentials {
        match env.get(&declaration.env).filter(|value| !value.is_empty()) {
            Some(value) => proxied.push(ProxiedCredential {
                declaration: declaration.clone(),
                value: value.clone(),
            }),
            None => warn(&format!(
                "credentialProxy: {} is not set in the engine environment; nothing to proxy",
                declaration.env
            )),
        }
    }
    if proxied.is_empty() {
        remember(holder, fingerprint);
        return Ok(());
    }
    let declarations: Vec<CredentialDeclaration> = proxied
        .iter()
        .map(|credential| credential.declaration.clone())
        .collect();
    let proxy = start_credential_proxy(proxied).await?;
    if !is_current(holder, generation) {
        proxy.close();
        return Ok(());
    }
    for declaration in &declarations {
        if let (Some(route), Some(base_url_env)) =
            (proxy.route_for(&declaration.env), &declaration.base_url_env)
        {
            set_route(holder, generation, base_url_env, &route);
        }
    }
    with_activations(|activations| {
        activations.proxies.insert(holder.to_owned(), proxy);
        activations
            .fingerprints
            .insert(holder.to_owned(), fingerprint);
    });
    Ok(())
}

fn remember(holder: &str, fingerprint: String) {
    with_activations(|activations| {
        activations
            .fingerprints
            .insert(holder.to_owned(), fingerprint);
    });
}

pub fn release_credential_proxy(holder: &str) {
    if let Some(proxy) = detach(holder) {
        proxy.close();
    }
}

/// The builtin's `session_start` step for the engine: the user's global `credentialProxy` setting against the
/// engine environment, problems and a failed start reported as diagnostics.
pub async fn activate_engine_credential_proxy(settings: Option<&Value>) {
    let env = maho_ext_api::process_env::vars();
    let app = crate::config::app_name();
    let warn = |message: &str| {
        maho_ext_api::diagnostic(&format!("[{app}:credential-proxy] {message}"));
    };
    if let Err(error) = activate_credential_proxy(ENGINE_HOLDER, settings, &env, &warn).await {
        maho_ext_api::diagnostic(&format!(
            "[{app}:credential-proxy] the proxy could not start ({error}); declared credentials stay withheld from agent commands"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn declared_credentials_are_withheld_and_routed_until_released() {
        let holder = "credential-proxy-runtime-test";
        let env: BTreeMap<String, String> = [
            ("CP_RUNTIME_TEST_KEY", "sk-runtime-secret"),
            ("CP_RUNTIME_TEST_COPY", "sk-runtime-secret"),
            ("PATH", "/usr/bin"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();
        let settings = json!({"credentials":[
            {"env":"CP_RUNTIME_TEST_KEY","origin":"https://api.example.com","baseUrlEnv":"CP_RUNTIME_TEST_BASE_URL"},
            {"env":"CP_RUNTIME_TEST_UNSET","origin":"https://api.example.com"}
        ]});
        let warnings = Mutex::new(Vec::new());
        let warn = |message: &str| warnings.lock().unwrap().push(message.to_owned());
        activate_credential_proxy(holder, Some(&settings), &env, &warn)
            .await
            .unwrap();
        let child: BTreeMap<String, String> = with_credential_proxy(env.clone());
        assert_eq!(child["CP_RUNTIME_TEST_KEY"], CREDENTIAL_PLACEHOLDER);
        assert_eq!(child["CP_RUNTIME_TEST_COPY"], CREDENTIAL_PLACEHOLDER);
        assert_eq!(child["CP_RUNTIME_TEST_UNSET"], CREDENTIAL_PLACEHOLDER);
        assert_eq!(child["PATH"], "/usr/bin");
        let route = child["CP_RUNTIME_TEST_BASE_URL"].clone();
        assert!(route.starts_with("http://127.0.0.1:"), "{route}");
        assert!(route.ends_with("/CP_RUNTIME_TEST_KEY"), "{route}");
        assert!(!child.values().any(|value| value == "sk-runtime-secret"));
        assert_eq!(
            warnings.lock().unwrap().clone(),
            vec![
                "credentialProxy: CP_RUNTIME_TEST_UNSET is not set in the engine environment; nothing to proxy"
            ]
        );

        activate_credential_proxy(holder, Some(&settings), &env, &warn)
            .await
            .unwrap();
        let again: BTreeMap<String, String> = with_credential_proxy(env.clone());
        assert_eq!(
            again["CP_RUNTIME_TEST_BASE_URL"], route,
            "an unchanged declaration keeps its proxy"
        );

        release_credential_proxy(holder);
        let released: BTreeMap<String, String> = with_credential_proxy(env.clone());
        assert_eq!(released["CP_RUNTIME_TEST_KEY"], "sk-runtime-secret");
        assert!(!released.contains_key("CP_RUNTIME_TEST_BASE_URL"));
    }
}
