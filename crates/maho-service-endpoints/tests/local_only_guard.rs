use maho_service_endpoints::{
    LocalOnlyGuard, LocalOnlyNetworkRefused, is_local_only_process, mark_local_only_process,
    refuse_if_local_only,
};

#[test]
fn an_unmarked_guard_lets_calls_through() {
    let guard = LocalOnlyGuard::new();
    assert!(!guard.is_marked());
    assert_eq!(guard.refuse("feedback.submit"), Ok(()));
}

#[test]
fn a_marked_guard_refuses_every_site_with_a_typed_error() {
    let guard = LocalOnlyGuard::new();
    guard.mark();
    for site in ["feedback.submit", "memory.account", "telemetry.export"] {
        let refused = guard.refuse(site).expect_err(site);
        assert_eq!(refused, LocalOnlyNetworkRefused::new(site));
        assert_eq!(refused.site(), site);
        assert_eq!(refused.code(), "local_only_network_refused");
        assert_eq!(
            refused.to_string(),
            format!("a local-only command reached the network at {site}")
        );
    }
    guard.mark();
    assert!(guard.is_marked());
}

#[test]
fn the_process_mark_is_shared_and_sticky() {
    mark_local_only_process();
    assert!(is_local_only_process());
    assert!(LocalOnlyGuard::process().is_marked());
    assert!(refuse_if_local_only("relay.peer").is_err());
}
