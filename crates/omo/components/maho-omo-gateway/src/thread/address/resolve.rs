//! The one resolver (design sections 2-5): `self`, a bare address, or `<device>/<thread>`.
//!
//! Upstream counterpart: `resolve.ts` (#14305). In mhc's no-account runtime (`ctx.remote === undefined` upstream),
//! a pinned reply answers `remote_unavailable`, `here/<thread>` asks this device's book (calling `local_for` with
//! the thread part when provided, #14305 review M2), and any other `<device>/<thread>` resolves as the whole text
//! against this device's book (a thread name on this device may contain `/`).

use maho_thread_address_core::{
    HERE, NotAskedReason, ParsedAddress, SilentDevice, SilentReason, device_not_asked,
    devices_silent, no_thread_on_device, parse_address,
};

use super::bare::resolve_bare;
use super::refusals::{ThreadRefusal, tool_refusal};
use super::types::{Found, ResolveContext, ResolveOptions, Resolved, ResolvedWhere};

fn finish(hit: Found) -> Resolved {
    let thread = hit.entry.thread_id.clone();
    Resolved {
        address: thread.clone(),
        thread,
        device: None,
        where_: ResolvedWhere::Here {
            entry: hit.entry.clone(),
        },
        entry: hit.entry,
        resolution: hit.resolution,
        all_scope: hit.all_scope,
    }
}

/// Resolves `text` (`self`, a bare address, or `<device>/<thread>`) against `ctx`.
pub fn resolve_address(
    ctx: &ResolveContext<'_>,
    text: &str,
    opts: &ResolveOptions<'_>,
) -> Result<Resolved, ThreadRefusal> {
    if let Some(pin) = opts.pin {
        let core = device_not_asked(NotAskedReason::Remote, pin.thread, pin.device, pin.device);
        return Err(tool_refusal(&core, None, None));
    }
    match parse_address(text) {
        ParsedAddress::SelfThread => resolve_bare(ctx, "self", opts).map(finish),
        ParsedAddress::Bare { .. } => resolve_bare(ctx, text, opts).map(finish),
        ParsedAddress::Device { device, thread, .. } => {
            if device == HERE {
                if let Some(local_for) = ctx.local_for {
                    let owned = local_for(&thread);
                    let sub = ResolveContext {
                        local: &owned,
                        ..*ctx
                    };
                    resolve_bare(&sub, &thread, opts).map(finish)
                } else {
                    resolve_bare(ctx, &thread, opts).map(finish)
                }
            } else {
                resolve_bare(ctx, text, opts).map(finish)
            }
        }
    }
}

/// Re-export helper for tests and callers building silence/thread refusals through `tool_refusal`.
pub fn silent_refusal(
    reason: SilentReason,
    devices: &[SilentDevice],
    thread: Option<&str>,
    answered: Option<&[String]>,
) -> ThreadRefusal {
    tool_refusal(&devices_silent(reason, devices), thread, answered)
}

/// Re-export helper for a thread-not-found refusal on a known device.
pub fn thread_missing_on_device(thread: &str, device_id: &str, device_name: &str) -> ThreadRefusal {
    tool_refusal(
        &no_thread_on_device(thread, device_id, device_name),
        None,
        None,
    )
}

#[cfg(test)]
mod tests {
    use maho_thread_address_core::{
        DeviceListing, NotAskedReason, ResolveDeviceOptions, SilentDevice, SilentReason,
        device_not_asked,
    };
    use serde_json::json;

    use super::*;
    use crate::thread::address::device::{AccountDevice, AccountDevices, resolve_account_device};
    use crate::thread::address::refusals::with_addresses;
    use crate::thread::address::types::{Pin, Resolution};
    use crate::thread::addressing::ThreadAddressEntry;
    use crate::thread::contracts_results::ThreadStatus;
    use crate::thread::errors::thread_tool_failure;
    use crate::thread::store_extension_session_ops::UNKNOWN_CALLER;

    const HERE_LIVE: &str = "0aa00001-0000-4000-8000-000000000001";
    const HERE_ENDED: &str = "0aa00002-0000-4000-8000-000000000002";
    const HERE_SHARED_1: &str = "0aa00003-0000-4000-8000-000000000003";
    const HERE_SHARED_2: &str = "0aa00005-0000-4000-8000-000000000005";
    const HERE_SLASH: &str = "0aa00004-0000-4000-8000-000000000004";

    fn entry(thread_id: &str, name: &str, route: Option<&str>, cwd: &str) -> ThreadAddressEntry {
        ThreadAddressEntry {
            thread_id: thread_id.to_owned(),
            name: name.to_owned(),
            cwd: cwd.to_owned(),
            status: ThreadStatus::Live,
            updated_at: None,
            session_ids: route.into_iter().map(str::to_owned).collect(),
        }
    }

    fn local_book() -> Vec<ThreadAddressEntry> {
        vec![
            entry(HERE_LIVE, "alpha", Some("rpc-3"), "/work/a"),
            entry(HERE_SLASH, "fix/login", Some("rpc-4"), "/work/a"),
            entry(HERE_ENDED, "", None, "/work/a"),
        ]
    }

    #[test]
    fn every_local_input_form_resolves_to_the_same_thread() {
        let book = local_book();
        let ctx = ResolveContext {
            caller_workspace_root: "/work/a",
            local: &book,
            local_for: None,
            caller_id: HERE_LIVE,
        };
        let opts = ResolveOptions::default();
        let cases = [
            (HERE_LIVE, HERE_LIVE, Resolution::Id),
            ("rpc-3", HERE_LIVE, Resolution::SessionId),
            ("alpha", HERE_LIVE, Resolution::Name),
            ("0aa00001", HERE_LIVE, Resolution::IdPrefix),
            ("self", HERE_LIVE, Resolution::SelfThread),
            (
                "here/0aa00001-0000-4000-8000-000000000001",
                HERE_LIVE,
                Resolution::Id,
            ),
            ("here/alpha", HERE_LIVE, Resolution::Name),
            ("here/rpc-3", HERE_LIVE, Resolution::SessionId),
            ("fix/login", HERE_SLASH, Resolution::Name),
        ];
        for (text, expected_thread, expected_res) in cases {
            let resolved = resolve_address(&ctx, text, &opts).expect(text);
            assert_eq!(resolved.thread, expected_thread, "{text}");
            assert_eq!(resolved.address, expected_thread, "{text}");
            assert_eq!(resolved.resolution, expected_res, "{text}");
            assert!(resolved.device.is_none(), "{text}");
        }
    }

    #[test]
    fn here_qualified_address_asks_local_for_with_the_thread_part_alone() {
        let all = local_book();
        let visible: Vec<ThreadAddressEntry> = all
            .iter()
            .filter(|item| item.thread_id != HERE_ENDED)
            .cloned()
            .collect();
        let local_for = |part: &str| -> Vec<ThreadAddressEntry> {
            if part == HERE_ENDED {
                all.clone()
            } else {
                visible.clone()
            }
        };
        let ctx = ResolveContext {
            caller_workspace_root: "/work/a",
            local: &visible,
            local_for: Some(&local_for),
            caller_id: HERE_LIVE,
        };
        let hit = resolve_address(
            &ctx,
            &format!("here/{HERE_ENDED}"),
            &ResolveOptions::default(),
        )
        .expect("here/<file id> resolves through local_for");
        assert_eq!(hit.thread, HERE_ENDED);
        assert_eq!(hit.resolution, Resolution::Id);
    }

    #[test]
    fn refusal_table_codes_and_details_match_design_section_5() {
        let mut book = local_book();
        book.push(entry(HERE_SHARED_1, "shared", None, "/work/a"));
        book.push(entry(HERE_SHARED_2, "shared", None, "/work/a"));
        let ctx = ResolveContext {
            caller_workspace_root: "/work/a",
            local: &book,
            local_for: None,
            caller_id: HERE_LIVE,
        };
        let opts = ResolveOptions::default();

        let empty = resolve_address(&ctx, "", &opts).expect_err("empty");
        assert_eq!(empty.code.as_str(), "invalid_arguments");

        let ctrl = resolve_address(&ctx, "a\u{0001}b", &opts).expect_err("control");
        assert_eq!(ctrl.code.as_str(), "invalid_arguments");

        let missing = resolve_address(&ctx, "no thread is named so", &opts).expect_err("missing");
        assert_eq!(missing.code.as_str(), "not_found");
        assert_eq!(missing.details, Some(json!({ "kind": "thread" })));

        let shared = resolve_address(&ctx, "shared", &opts).expect_err("shared");
        assert_eq!(shared.code.as_str(), "ambiguous_target");
        assert_eq!(
            shared.details.as_ref().and_then(|d| d.get("kind")),
            Some(&json!("thread"))
        );

        let unknown_caller = ResolveContext {
            caller_id: UNKNOWN_CALLER,
            ..ctx
        };
        let self_err = resolve_address(&unknown_caller, "self", &opts).expect_err("unknown caller");
        assert_eq!(self_err.code.as_str(), "caller_context_missing");

        let other_ws = ResolveContext {
            caller_workspace_root: "/work/other",
            ..ctx
        };
        let denied = resolve_address(&other_ws, "alpha", &opts).expect_err("scope denied");
        assert_eq!(denied.code.as_str(), "scope_denied");

        let no_root = ResolveContext {
            caller_workspace_root: "   ",
            ..ctx
        };
        let fuzzy_opts = ResolveOptions {
            fuzzy: true,
            ..opts
        };
        let fuzzy_err = resolve_address(&no_root, "alpha", &fuzzy_opts).expect_err("fuzzy no root");
        assert_eq!(fuzzy_err.code.as_str(), "caller_context_missing");

        let pinned_opts = ResolveOptions {
            pin: Some(Pin {
                device: "b7c41e09d2a64f1e",
                thread: "0bb00001-0000-4000-8000-000000000001",
            }),
            ..opts
        };
        let pin_err = resolve_address(&ctx, "0bb00001-0000-4000-8000-000000000001", &pinned_opts)
            .expect_err("pin without remote");
        assert_eq!(pin_err.code.as_str(), "host_unavailable");
        assert_eq!(
            pin_err.details.as_ref().and_then(|d| d.get("reason")),
            Some(&json!("remote_unavailable"))
        );
    }

    #[test]
    fn account_device_resolution_and_tool_refusal_vocabulary() {
        let account = AccountDevices {
            here: Some("a11ce0de5e1f0123".to_owned()),
            devices: vec![
                AccountDevice {
                    device: "a11ce0de5e1f0123".to_owned(),
                    label: "this mac".to_owned(),
                    online: true,
                    last_seen_ms: None,
                },
                AccountDevice {
                    device: "b7c41e09d2a64f1e".to_owned(),
                    label: "Studio Laptop".to_owned(),
                    online: true,
                    last_seen_ms: None,
                },
                AccountDevice {
                    device: "b7c4ffff00000000".to_owned(),
                    label: "Studio Laptop".to_owned(),
                    online: true,
                    last_seen_ms: None,
                },
                AccountDevice {
                    device: "c0ffee5a1d2b3c4d".to_owned(),
                    label: "attic".to_owned(),
                    online: false,
                    last_seen_ms: Some(1_699_000_000_000),
                },
            ],
            listing_of: None,
        };

        let here = resolve_account_device(
            &account,
            "here",
            ResolveDeviceOptions {
                online: true,
                exact_id: false,
            },
        )
        .expect("here");
        assert!(here.here);
        assert_eq!(here.listing, DeviceListing::Listed);

        let unknown = resolve_account_device(
            &account,
            "nowhere-device",
            ResolveDeviceOptions {
                online: false,
                exact_id: false,
            },
        )
        .expect_err("unknown");
        assert_eq!(unknown.code.as_str(), "not_found");
        assert!(unknown.message.contains("No device named nowhere-device"));

        let share_name = resolve_account_device(
            &account,
            "Studio Laptop",
            ResolveDeviceOptions {
                online: false,
                exact_id: false,
            },
        )
        .expect_err("share name");
        assert_eq!(share_name.code.as_str(), "ambiguous_target");
        assert!(
            share_name
                .message
                .contains("2 online devices are named Studio Laptop")
        );

        let share_prefix = resolve_account_device(
            &account,
            "b7c4",
            ResolveDeviceOptions {
                online: false,
                exact_id: false,
            },
        )
        .expect_err("short prefix is not_found");
        assert_eq!(share_prefix.code.as_str(), "not_found");

        let offline = resolve_account_device(
            &account,
            "attic",
            ResolveDeviceOptions {
                online: true,
                exact_id: false,
            },
        )
        .expect_err("offline");
        assert_eq!(offline.code.as_str(), "device_offline");
        assert_eq!(
            offline.details.as_ref().and_then(|d| d.get("last_seen")),
            Some(&json!(1_699_000_000_000_i64))
        );

        let exact_miss = resolve_account_device(
            &account,
            "this mac",
            ResolveDeviceOptions {
                online: true,
                exact_id: true,
            },
        )
        .expect_err("exact_id refuses label");
        assert_eq!(exact_miss.code.as_str(), "not_found");
    }

    #[test]
    fn silent_and_not_asked_refusals_keep_reasons_and_misplaced_file_hint() {
        let incomplete = silent_refusal(
            SilentReason::ListingIncomplete,
            &[SilentDevice {
                id: "b7c41e09d2a64f1e".to_owned(),
                name: "Studio Laptop".to_owned(),
                reason: None,
            }],
            None,
            None,
        );
        assert_eq!(incomplete.code.as_str(), "host_unavailable");
        assert!(
            incomplete
                .message
                .contains("Studio Laptop (b7c41e09d2a64f1e)")
        );

        let answered = vec!["a11ce0de5e1f0123".to_owned()];
        let unanswered = silent_refusal(
            SilentReason::DeviceUnanswered,
            &[
                SilentDevice {
                    id: "b7c41e09d2a64f1e".to_owned(),
                    name: "Studio Laptop".to_owned(),
                    reason: Some("lookup_timeout".to_owned()),
                },
                SilentDevice {
                    id: "c1a55e0000000001".to_owned(),
                    name: "closet".to_owned(),
                    reason: Some("host_not_answering".to_owned()),
                },
            ],
            Some(HERE_LIVE),
            Some(&answered),
        );
        assert_eq!(unanswered.code.as_str(), "host_unavailable");
        assert!(unanswered.message.contains("was too slow for the lookup"));
        assert!(unanswered.message.contains("did not answer"));
        assert!(
            unanswered
                .next_action
                .contains("its file is not where that device looks for sessions")
        );
        assert_eq!(
            unanswered
                .details
                .as_ref()
                .and_then(|d| d.get("device_reasons"))
                .and_then(|r| r.get("c1a55e0000000001")),
            Some(&json!("host_not_answering"))
        );

        let wake = tool_refusal(
            &device_not_asked(
                NotAskedReason::Wake,
                HERE_LIVE,
                "b7c41e09d2a64f1e",
                "Studio Laptop",
            ),
            None,
            None,
        );
        assert_eq!(wake.code.as_str(), "host_unavailable");
        assert!(wake.message.contains("cannot carry a wake"));

        let missing_on_b = thread_missing_on_device("no-such", "b7c41e09d2a64f1e", "Studio Laptop");
        assert_eq!(missing_on_b.code.as_str(), "not_found");
        assert!(
            missing_on_b
                .message
                .contains("No thread no-such on Studio Laptop")
        );
    }

    #[test]
    fn with_addresses_enriches_candidates_and_preserves_keyless_row_keys() {
        let raw = thread_tool_failure(
            "ambiguous_target",
            "shared",
            "pick one",
            Some(json!({
                "candidates": [
                    {
                        "thread": HERE_SHARED_1,
                        "name": "shared",
                        "cwd": "/work/a",
                        "preview": "",
                        "updatedAt": null,
                        "state": "live",
                        "device": "here",
                    },
                    {
                        "thread": "b7c41e09d2a64f1e/c2Vzc2lvbi1rZXktMDAwMQ",
                        "name": "shared",
                        "cwd": "/work/b",
                        "preview": "",
                        "updatedAt": null,
                        "state": "live",
                        "device": "b7c41e09d2a64f1e",
                    }
                ]
            })),
        );
        let enriched = with_addresses(raw, Some("a11ce0de5e1f0123"), |device| match device {
            "a11ce0de5e1f0123" => Some("this mac".to_owned()),
            "b7c41e09d2a64f1e" => Some("Studio Laptop".to_owned()),
            _ => None,
        });
        let details = enriched.details.expect("details");
        assert_eq!(details["kind"], "thread");
        let candidates = details["candidates"].as_array().expect("candidates");
        assert_eq!(
            candidates[0]["address"],
            format!("a11ce0de5e1f0123/{HERE_SHARED_1}")
        );
        assert_eq!(candidates[0]["device_name"], "this mac");
        assert_eq!(
            candidates[1]["address"],
            "b7c41e09d2a64f1e/c2Vzc2lvbi1rZXktMDAwMQ"
        );
        assert_eq!(candidates[1]["device_name"], "Studio Laptop");
    }
}
