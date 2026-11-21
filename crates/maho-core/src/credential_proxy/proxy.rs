//! Port of senpi `packages/coding-agent/src/core/credential-proxy/proxy.ts` (6d4426a28; senpi#6828).
//!
//! The loopback credential proxy (senpi#6820). A child calls `http://127.0.0.1:<port>/<capability>/<env name>/<path>`;
//! the proxy forwards to that credential's fixed origin with the header set from the value it holds. Security rules:
//! the upstream host never comes from the request, redirects are returned rather than followed (a followed redirect
//! would carry the header to another host), and no error text the child can read includes the value.
use super::declarations::{CredentialDeclaration, has_unsafe_path_segment};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, StreamBody, combinators::UnsyncBoxBody};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct ProxiedCredential {
    pub declaration: CredentialDeclaration,
    pub value: String,
}

const MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;
const DROPPED_REQUEST_HEADERS: [&str; 11] = [
    "accept-encoding",
    "connection",
    "content-length",
    "host",
    "keep-alive",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];
const DROPPED_RESPONSE_HEADERS: [&str; 5] = [
    "connection",
    "content-encoding",
    "content-length",
    "keep-alive",
    "transfer-encoding",
];

type ProxyBody = UnsyncBoxBody<Bytes, std::io::Error>;

/// A listening proxy. Dropping it (or [`CredentialProxy::close`]) stops accepting and ends open connections.
pub struct CredentialProxy {
    port: u16,
    capability: String,
    envs: Vec<String>,
    server: tokio::task::JoinHandle<()>,
}

impl CredentialProxy {
    /// The route a child uses for one credential: `http://127.0.0.1:<port>/<capability>/<env>`.
    pub fn route_for(&self, env: &str) -> Option<String> {
        self.envs
            .iter()
            .any(|name| name == env)
            .then(|| format!("http://127.0.0.1:{}/{}/{env}", self.port, self.capability))
    }

    pub fn close(&self) {
        self.server.abort();
    }

    /// False once the server task ended (closed, or its runtime shut down).
    pub fn is_running(&self) -> bool {
        !self.server.is_finished()
    }
}

impl Drop for CredentialProxy {
    fn drop(&mut self) {
        self.server.abort();
    }
}

struct ProxyState {
    capability: Vec<u8>,
    by_env: HashMap<String, ProxiedCredential>,
    client: reqwest::Client,
}

/// Binds the proxy on loopback; fails when it cannot listen, so the caller never falls back to passing values.
pub async fn start_credential_proxy(
    credentials: Vec<ProxiedCredential>,
) -> std::io::Result<CredentialProxy> {
    let capability = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let envs: Vec<String> = credentials
        .iter()
        .map(|credential| credential.declaration.env.clone())
        .collect();
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(std::io::Error::other)?;
    let state = Arc::new(ProxyState {
        capability: capability.clone().into_bytes(),
        by_env: credentials
            .into_iter()
            .map(|credential| (credential.declaration.env.clone(), credential))
            .collect(),
        client,
    });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let server = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else { break; };
                    let state = Arc::clone(&state);
                    connections.spawn(async move {
                        let service = hyper::service::service_fn(move |request| {
                            serve(request, Arc::clone(&state))
                        });
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                            .await;
                    });
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    });
    Ok(CredentialProxy {
        port,
        capability,
        envs,
        server,
    })
}

fn full(bytes: impl Into<Bytes>) -> ProxyBody {
    Full::new(bytes.into())
        .map_err(|never: std::convert::Infallible| match never {})
        .boxed_unsync()
}

fn refuse(status: u16, reason: &str) -> hyper::Response<ProxyBody> {
    let mut response = hyper::Response::new(full(format!("credential proxy: {reason}\n")));
    *response.status_mut() =
        hyper::StatusCode::from_u16(status).unwrap_or(hyper::StatusCode::BAD_REQUEST);
    response.headers_mut().insert(
        hyper::header::CONTENT_TYPE,
        hyper::header::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

/// Constant-time comparison of the route's capability segment.
fn same_capability(given: &[u8], expected: &[u8]) -> bool {
    given.len() == expected.len()
        && given
            .iter()
            .zip(expected)
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

/// Whether `path` (below the credential's origin) may be forwarded for `credential`.
pub fn allowed_path(path: &str, credential: &CredentialDeclaration) -> bool {
    if has_unsafe_path_segment(path) {
        return false;
    }
    credential.path_prefixes.as_ref().is_none_or(|prefixes| {
        prefixes.iter().any(|prefix| {
            path == prefix
                || if prefix.ends_with('/') {
                    path.starts_with(prefix.as_str())
                } else {
                    path.starts_with(&format!("{prefix}/"))
                }
        })
    })
}

/// The proxy's route split: `/<capability>/<env>/<rest...>` into `(capability, env, "/<rest>")`.
pub fn split_route(path: &str) -> Option<(&str, &str, String)> {
    let mut segments = path.strip_prefix('/')?.splitn(3, '/');
    let given = segments.next().unwrap_or_default();
    let env = segments.next().unwrap_or_default();
    let rest = segments.next().unwrap_or_default();
    Some((given, env, format!("/{rest}")))
}

async fn serve(
    request: hyper::Request<hyper::body::Incoming>,
    state: Arc<ProxyState>,
) -> Result<hyper::Response<ProxyBody>, std::convert::Infallible> {
    Ok(forward(request, &state).await)
}

async fn forward(
    request: hyper::Request<hyper::body::Incoming>,
    state: &ProxyState,
) -> hyper::Response<ProxyBody> {
    let (parts, mut body) = request.into_parts();
    let Some(path_and_query) = parts
        .uri
        .path_and_query()
        .filter(|_| parts.uri.scheme().is_none() && parts.uri.authority().is_none())
    else {
        return refuse(400, "only origin-form requests are accepted");
    };
    let path = path_and_query.path();
    let query = path_and_query
        .query()
        .map_or_else(String::new, |query| format!("?{query}"));
    let Some((given, env, upstream_path)) = split_route(path) else {
        return refuse(400, "only origin-form requests are accepted");
    };
    let credential = state.by_env.get(env);
    let Some(credential) =
        credential.filter(|_| same_capability(given.as_bytes(), &state.capability))
    else {
        return refuse(403, "unknown route");
    };
    if !allowed_path(&upstream_path, &credential.declaration) {
        return refuse(403, "path not allowed for this credential");
    }
    let mut raw = Vec::new();
    if parts.method != hyper::Method::GET && parts.method != hyper::Method::HEAD {
        while let Some(frame) = body.frame().await {
            let Ok(frame) = frame else {
                return refuse(400, "request body could not be read");
            };
            if let Ok(data) = frame.into_data() {
                if data.len() > MAX_REQUEST_BYTES.saturating_sub(raw.len()) {
                    return refuse(413, "request body too large");
                }
                raw.extend_from_slice(&data);
            }
        }
    }
    let Ok(method) = reqwest::Method::from_bytes(parts.method.as_str().as_bytes()) else {
        return refuse(400, "unsupported method");
    };
    let mut headers = reqwest::header::HeaderMap::new();
    for (name, value) in &parts.headers {
        let lower = name.as_str().to_ascii_lowercase();
        if DROPPED_REQUEST_HEADERS.contains(&lower.as_str())
            || lower == credential.declaration.header
        {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(lower.as_bytes()),
            reqwest::header::HeaderValue::from_bytes(value.as_bytes()),
        ) {
            headers.append(name, value);
        }
    }
    let secret = match &credential.declaration.scheme {
        Some(scheme) => format!("{scheme} {}", credential.value),
        None => credential.value.clone(),
    };
    let (Ok(name), Ok(mut value)) = (
        reqwest::header::HeaderName::from_bytes(credential.declaration.header.as_bytes()),
        reqwest::header::HeaderValue::from_str(&secret),
    ) else {
        return refuse(502, "upstream request failed");
    };
    value.set_sensitive(true);
    headers.insert(name, value);
    headers.insert(
        reqwest::header::ACCEPT_ENCODING,
        reqwest::header::HeaderValue::from_static("identity"),
    );
    let target = format!(
        "{}{}{query}",
        credential.declaration.origin,
        if upstream_path == "/" {
            ""
        } else {
            upstream_path.as_str()
        }
    );
    let mut upstream_request = state.client.request(method, target).headers(headers);
    if parts.method != hyper::Method::GET && parts.method != hyper::Method::HEAD {
        upstream_request = upstream_request.body(raw);
    }
    let Ok(upstream) = upstream_request.send().await else {
        return refuse(502, "upstream request failed");
    };
    relay(upstream)
}

fn relay(upstream: reqwest::Response) -> hyper::Response<ProxyBody> {
    let mut builder = hyper::Response::builder().status(upstream.status().as_u16());
    for (name, value) in upstream.headers() {
        if DROPPED_RESPONSE_HEADERS.contains(&name.as_str()) {
            continue;
        }
        builder = builder.header(name.as_str(), value.as_bytes());
    }
    let chunks = futures::stream::unfold(Some(upstream), |upstream| async move {
        let mut upstream = upstream?;
        match upstream.chunk().await {
            Ok(Some(chunk)) => Some((Ok(hyper::body::Frame::data(chunk)), Some(upstream))),
            Ok(None) => None,
            Err(error) => Some((Err(std::io::Error::other(error)), None)),
        }
    });
    builder
        .body(StreamBody::new(chunks).boxed_unsync())
        .unwrap_or_else(|_| refuse(502, "upstream response could not be relayed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declaration(prefixes: Option<Vec<&str>>) -> CredentialDeclaration {
        CredentialDeclaration {
            env: "KEY".into(),
            origin: "https://example.com".into(),
            header: "authorization".into(),
            scheme: Some("Bearer".into()),
            base_url_env: None,
            path_prefixes: prefixes.map(|list| list.into_iter().map(str::to_owned).collect()),
        }
    }

    #[test]
    fn routes_split_into_capability_env_and_upstream_path() {
        assert_eq!(
            split_route("/cap/KEY/v1/chat"),
            Some(("cap", "KEY", "/v1/chat".to_owned()))
        );
        assert_eq!(
            split_route("/cap/KEY"),
            Some(("cap", "KEY", "/".to_owned()))
        );
        assert_eq!(split_route("cap/KEY"), None);
    }

    #[test]
    fn path_prefixes_bound_the_forwarded_paths() {
        let open = declaration(None);
        assert!(allowed_path("/anything", &open));
        assert!(!allowed_path("/a/../b", &open));
        let bounded = declaration(Some(vec!["/v1", "/files/"]));
        assert!(allowed_path("/v1", &bounded));
        assert!(allowed_path("/v1/chat", &bounded));
        assert!(!allowed_path("/v10", &bounded));
        assert!(allowed_path("/files/x", &bounded));
        assert!(!allowed_path("/admin", &bounded));
    }

    /// A loopback upstream that reports each request's path, query and authorization header.
    async fn upstream() -> (
        u16,
        tokio::sync::mpsc::UnboundedReceiver<(String, Option<String>, Option<String>)>,
    ) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let sender = sender.clone();
                tokio::spawn(async move {
                    let service = hyper::service::service_fn(
                        move |request: hyper::Request<hyper::body::Incoming>| {
                            let path = request.uri().path().to_owned();
                            let query = request.uri().query().map(str::to_owned);
                            let authorization = request
                                .headers()
                                .get(hyper::header::AUTHORIZATION)
                                .and_then(|value| value.to_str().ok())
                                .map(str::to_owned);
                            let redirect = path == "/v1/redirect";
                            let _ = sender.send((path, query, authorization));
                            async move {
                                let mut response = hyper::Response::new(Full::new(
                                    Bytes::from_static(b"upstream-ok"),
                                ));
                                if redirect {
                                    *response.status_mut() = hyper::StatusCode::FOUND;
                                    response.headers_mut().insert(
                                        hyper::header::LOCATION,
                                        hyper::header::HeaderValue::from_static(
                                            "https://elsewhere.invalid/",
                                        ),
                                    );
                                }
                                Ok::<_, std::convert::Infallible>(response)
                            }
                        },
                    );
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        (port, receiver)
    }

    #[tokio::test]
    async fn the_proxy_sets_the_credential_for_its_one_origin_and_refuses_everything_else() {
        let (port, mut seen) = upstream().await;
        let mut declaration = declaration(Some(vec!["/v1"]));
        declaration.origin = format!("http://127.0.0.1:{port}");
        let proxy = start_credential_proxy(vec![ProxiedCredential {
            declaration,
            value: "sk-proxy-secret".into(),
        }])
        .await
        .unwrap();
        let route = proxy.route_for("KEY").unwrap();
        assert_eq!(proxy.route_for("OTHER"), None);
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();

        let response = client
            .get(format!("{route}/v1/models?limit=1"))
            .header("authorization", "Bearer forged-by-child")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(response.text().await.unwrap(), "upstream-ok");
        assert_eq!(
            seen.recv().await.unwrap(),
            (
                "/v1/models".to_owned(),
                Some("limit=1".to_owned()),
                Some("Bearer sk-proxy-secret".to_owned())
            )
        );

        let redirect = client
            .get(format!("{route}/v1/redirect"))
            .send()
            .await
            .unwrap();
        assert_eq!(redirect.status().as_u16(), 302);
        assert_eq!(seen.recv().await.unwrap().0, "/v1/redirect");

        let outside = client.get(format!("{route}/admin")).send().await.unwrap();
        assert_eq!(outside.status().as_u16(), 403);
        let text = outside.text().await.unwrap();
        assert!(!text.contains("sk-proxy-secret"), "{text}");
        let forged = route.replacen("/KEY", "/OTHER", 1);
        let unknown = client
            .get(format!("{forged}/v1/models"))
            .send()
            .await
            .unwrap();
        assert_eq!(unknown.status().as_u16(), 403);
        let wrong_capability = format!(
            "http://127.0.0.1:{}/not-the-capability/KEY/v1/models",
            proxy.port
        );
        let refused = client.get(wrong_capability).send().await.unwrap();
        assert_eq!(refused.status().as_u16(), 403);
        proxy.close();
        assert!(
            seen.try_recv().is_err(),
            "refused requests never reach the upstream"
        );
    }

    #[test]
    fn capabilities_compare_whole() {
        assert!(same_capability(b"abc", b"abc"));
        assert!(!same_capability(b"abd", b"abc"));
        assert!(!same_capability(b"ab", b"abc"));
    }
}
