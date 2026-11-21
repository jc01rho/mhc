use std::collections::BTreeMap;

use maho_service_endpoints::{
    EndpointRefusal, EndpointSource, OMO_SERVICE_API_URL_ENV, SERVICE_API_URL_ENV, check_peer_host,
    check_service_origin, resolve_service_api, service_trace_hosts,
};

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: BTreeMap<String, String> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    move |key: &str| map.get(key).cloned()
}

#[test]
fn nothing_configured_is_local_mode() {
    assert_eq!(resolve_service_api(None, env(&[])), Ok(None));
    assert_eq!(resolve_service_api(Some("  "), env(&[])), Ok(None));
    assert_eq!(
        resolve_service_api(
            None,
            env(&[(SERVICE_API_URL_ENV, ""), (OMO_SERVICE_API_URL_ENV, " ")])
        ),
        Ok(None)
    );
}

#[test]
fn explicit_wins_over_maho_env_which_wins_over_omo_env() {
    let all = env(&[
        (SERVICE_API_URL_ENV, "https://maho.example"),
        (OMO_SERVICE_API_URL_ENV, "https://omo.example"),
    ]);
    let explicit = resolve_service_api(Some("https://explicit.example/"), &all)
        .expect("valid")
        .expect("configured");
    assert_eq!(explicit.origin, "https://explicit.example");
    assert_eq!(explicit.source, EndpointSource::Explicit);

    let maho = resolve_service_api(None, &all)
        .expect("valid")
        .expect("configured");
    assert_eq!(maho.origin, "https://maho.example");
    assert_eq!(maho.source, EndpointSource::Env(SERVICE_API_URL_ENV));

    let omo = resolve_service_api(
        None,
        env(&[(OMO_SERVICE_API_URL_ENV, "https://omo.example")]),
    )
    .expect("valid")
    .expect("configured");
    assert_eq!(omo.source, EndpointSource::Env(OMO_SERVICE_API_URL_ENV));
    assert_eq!(omo.join("/v1/reports"), "https://omo.example/v1/reports");
}

#[test]
fn a_refused_value_names_its_setting_and_never_falls_back() {
    let error = resolve_service_api(
        None,
        env(&[
            (SERVICE_API_URL_ENV, "http://192.168.1.10"),
            (OMO_SERVICE_API_URL_ENV, "https://omo.example"),
        ]),
    )
    .expect_err("plain http to a LAN host");
    assert_eq!(error.source, EndpointSource::Env(SERVICE_API_URL_ENV));
    assert_eq!(
        error.refusal,
        EndpointRefusal::InsecureHost("192.168.1.10".into())
    );
    assert!(error.to_string().starts_with("MAHO_SERVICE_API_URL: "));
}

#[test]
fn origin_matrix() {
    let accepted = [
        ("https://api.example.com", "https://api.example.com"),
        ("https://api.example.com/", "https://api.example.com"),
        (
            "https://api.example.com:8443",
            "https://api.example.com:8443",
        ),
        ("http://127.0.0.1:8080", "http://127.0.0.1:8080"),
        ("http://localhost:3000/", "http://localhost:3000"),
        ("http://[::1]:9000", "http://[::1]:9000"),
        ("http://100.101.102.103:7000", "http://100.101.102.103:7000"),
        ("http://box.tail1234.ts.net", "http://box.tail1234.ts.net"),
    ];
    for (input, origin) in accepted {
        assert_eq!(
            check_service_origin(input).as_deref(),
            Ok(origin),
            "{input}"
        );
    }
    let refused = [
        ("not a url", "endpoint_unparsable"),
        ("ftp://api.example.com", "endpoint_scheme"),
        ("http://api.example.com", "endpoint_insecure"),
        ("http://10.0.0.5", "endpoint_insecure"),
        ("http://192.168.0.2:80", "endpoint_insecure"),
        ("http://100.128.0.1", "endpoint_insecure"),
        ("https://user@api.example.com", "endpoint_credentials"),
        (
            "https://user:secret@api.example.com",
            "endpoint_credentials",
        ),
        ("https://api.example.com/v1", "endpoint_not_origin"),
        ("https://api.example.com/?x=1", "endpoint_not_origin"),
        ("https://api.example.com/#frag", "endpoint_not_origin"),
    ];
    for (input, code) in refused {
        let refusal = check_service_origin(input).expect_err(input);
        assert_eq!(refusal.code(), code, "{input}");
    }
}

#[test]
fn peer_hosts_carry_plain_text_only_on_loopback_or_a_tailnet() {
    for host in [
        "127.0.0.1",
        "::1",
        "localhost",
        "100.64.0.9",
        "laptop.tailabcd.ts.net",
    ] {
        assert_eq!(check_peer_host(host), Ok(()), "{host}");
    }
    for host in ["192.168.1.4", "10.1.2.3", "example.com", "8.8.8.8"] {
        assert!(matches!(
            check_peer_host(host),
            Err(EndpointRefusal::InsecureHost(_))
        ));
    }
    assert_eq!(check_peer_host(" "), Err(EndpointRefusal::MissingHost));
}

#[test]
fn trace_hosts_name_only_a_configured_service() {
    assert!(service_trace_hosts(env(&[])).is_empty());
    assert_eq!(
        service_trace_hosts(env(&[(
            OMO_SERVICE_API_URL_ENV,
            "https://API.Example.com/"
        )])),
        vec!["api.example.com".to_owned()]
    );
    assert!(service_trace_hosts(env(&[(SERVICE_API_URL_ENV, "http://evil.example")])).is_empty());
}
