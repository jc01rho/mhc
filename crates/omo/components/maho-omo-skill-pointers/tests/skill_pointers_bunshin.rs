use crate::support;

use maho_ext_api::*;
use maho_omo_skill_pointers::*;
use support::*;

#[test]
fn given_fleet_or_other_device_wording_when_matched_then_omo_relay_pointer_target_fires() {
    let triggers = [
        "run this on builder via bunshin",
        "list my fleet",
        "run uptime on our fleet",
        "restart the fleet hosts",
        "reboot the fleet box",
        "check disk on my fleets",
        "restart fleet-node-3",
        "deploy to our fleet and verify",
        "is my fleet up?",
        "run it on my fleet, then report",
        "check the machine mesh",
        "machine-mesh health",
        "check disk on my fleet\nthen report",
        "my fleet\nrun df",
        "run this on my other machine",
        "check disk on our other devices",
        "fan this out with omo-relay",
    ];
    for text in triggers {
        assert_eq!(
            matched_skill_pointer_names(text),
            vec!["omo-relay"],
            "trigger '{text}' failed"
        );
    }
}

#[test]
fn given_near_miss_fleet_wording_when_matched_then_nothing_fires() {
    let misses = [
        "fleeting thought",
        "Fleet status please",
        "the delivery fleet schedule",
        "the fleet-dashboard css",
        "update our fleet management UI",
        "our fleet of delivery trucks",
        "my fleets of ships",
        "our fleet's fuel costs",
        "our fleet size grew last year",
        "our fleet is aging and needs new trucks",
        "our fleet in the harbor",
        "our fleets are docked",
        "the fleet hosts a regatta",
        "bunshins",
        "the mesh network",
        "machine learning",
        "`bunshin` in a code span",
        "mybunshin_helper",
        "my other machine learning project",
        "the other machine in the lab",
    ];
    for text in misses {
        let empty: Vec<&str> = Vec::new();
        assert_eq!(
            matched_skill_pointer_names(text),
            empty,
            "near-miss '{text}' should not match"
        );
    }
}

#[tokio::test]
async fn given_an_interactive_prompt_naming_the_fleet_when_dispatched_then_one_hidden_omo_relay_pointer_is_injected()
 {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(
        &harness,
        "list my fleet",
        InputSource::Interactive,
        None,
        "test-session",
    )
    .await;
    expect_pointer_injections(
        &harness,
        &result,
        &[ExpectedPointer {
            custom_type: OMO_RELAY_CUSTOM_TYPE,
            skill_name: "omo-relay",
        }],
    );
    let content = injected_content(&harness, OMO_RELAY_CUSTOM_TYPE);
    assert!(content.starts_with("<omo-omo-relay-pointer>"));
    assert!(content.contains("omo-relay/SKILL.md"));
    assert!(content.contains("scripts/node/bunshin-node.mjs"));
}

#[tokio::test]
async fn given_the_plain_word_fleet_in_an_unrelated_request_when_dispatched_then_no_hidden_pointer_is_injected()
 {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(
        &harness,
        "the fleet dashboard CSS is broken",
        InputSource::Interactive,
        None,
        "test-session",
    )
    .await;
    expect_no_injection(&harness, &result);
}

#[tokio::test]
async fn given_a_device_operations_request_when_dispatched_then_the_hidden_omo_relay_pointer_is_injected()
 {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(
        &harness,
        "check disk usage on every machine in my fleet",
        InputSource::Interactive,
        None,
        "test-session",
    )
    .await;
    expect_pointer_injections(
        &harness,
        &result,
        &[ExpectedPointer {
            custom_type: OMO_RELAY_CUSTOM_TYPE,
            skill_name: "omo-relay",
        }],
    );
}

#[tokio::test]
async fn given_a_prompt_without_fleet_wording_when_dispatched_then_no_omo_relay_pointer_is_injected()
 {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(
        &harness,
        "rename this variable",
        InputSource::Interactive,
        None,
        "test-session",
    )
    .await;
    expect_no_injection(&harness, &result);
}
