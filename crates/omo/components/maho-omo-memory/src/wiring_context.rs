use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError, Weak};

use maho_ext_api::{ExtensionContext, ExtensionUi, SessionManager};

#[derive(Clone, Debug, PartialEq, Eq)]
struct PublishedLine {
    text: Option<String>,
    request: u64,
}

#[derive(Default)]
struct HostStatusState {
    requests: u64,
    cleared_at: BTreeMap<String, u64>,
    lines: BTreeMap<String, PublishedLine>,
}

pub type StatusDeliver = Arc<dyn Fn(&str, Option<&str>) -> Result<(), String> + Send + Sync>;
pub type StatusAllow = Arc<dyn Fn() -> bool + Send + Sync>;

type HostStatusEntry = (Weak<dyn ExtensionUi>, Arc<Mutex<HostStatusState>>);

static HOST_STATUSES: LazyLock<Mutex<Vec<HostStatusEntry>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

fn host_status_for_ui(ui: &Arc<dyn ExtensionUi>) -> Arc<Mutex<HostStatusState>> {
    let weak = Arc::downgrade(ui);
    let mut entries = HOST_STATUSES.lock().unwrap_or_else(PoisonError::into_inner);
    entries.retain(|(existing, _)| existing.strong_count() > 0);
    if let Some((_, state)) = entries.iter().find(|(existing, _)| {
        existing
            .upgrade()
            .is_some_and(|_| std::ptr::addr_eq(existing.as_ptr(), weak.as_ptr()))
    }) {
        return Arc::clone(state);
    }
    let state = Arc::new(Mutex::new(HostStatusState::default()));
    entries.push((weak, Arc::clone(&state)));
    state
}

/// Per-host status publishing handle (#13046, #14825) keeping an unchanged line unpublished,
/// dropping stale sequenced refreshes, and giving clears their own boundary.
#[derive(Clone)]
pub struct StatusHandle {
    host: Arc<Mutex<HostStatusState>>,
    deliver: StatusDeliver,
    allow: StatusAllow,
    request: Option<u64>,
}

impl StatusHandle {
    pub fn from_extension_ui(ui: Arc<dyn ExtensionUi>) -> Self {
        let host = host_status_for_ui(&ui);
        Self {
            host,
            deliver: Arc::new(move |key, text| {
                ui.set_status(key, text);
                Ok(())
            }),
            allow: Arc::new(|| true),
            request: None,
        }
    }

    pub fn from_fallible(deliver: StatusDeliver) -> Self {
        Self {
            host: Arc::new(Mutex::new(HostStatusState::default())),
            deliver,
            allow: Arc::new(|| true),
            request: None,
        }
    }

    pub fn try_set_status(&self, key: &str, text: Option<&str>) -> Result<(), String> {
        if !(self.allow)() {
            return Ok(());
        }
        let mut state = self.host.lock().unwrap_or_else(PoisonError::into_inner);
        let last = state.lines.get(key).cloned();
        let Some(text) = text else {
            (self.deliver)(key, None)?;
            let requests = state.requests;
            state.cleared_at.insert(key.to_owned(), requests);
            state.lines.insert(
                key.to_owned(),
                PublishedLine {
                    text: None,
                    request: last.map_or(0, |line| line.request),
                },
            );
            return Ok(());
        };
        if let Some(request) = self.request {
            if request <= state.cleared_at.get(key).copied().unwrap_or(0) {
                return Ok(());
            }
            if last.as_ref().is_some_and(|line| request < line.request) {
                return Ok(());
            }
        }
        let stamp = self
            .request
            .unwrap_or_else(|| last.as_ref().map_or(0, |line| line.request));
        if last.as_ref().and_then(|line| line.text.as_deref()) == Some(text) {
            state.lines.insert(
                key.to_owned(),
                PublishedLine {
                    text: Some(text.to_owned()),
                    request: last.map_or(stamp, |line| line.request.max(stamp)),
                },
            );
            return Ok(());
        }
        (self.deliver)(key, Some(text))?;
        state.lines.insert(
            key.to_owned(),
            PublishedLine {
                text: Some(text.to_owned()),
                request: last.map_or(stamp, |line| line.request.max(stamp)),
            },
        );
        Ok(())
    }

    pub fn set_status(&self, key: &str, text: Option<&str>) {
        let _ = self.try_set_status(key, text);
    }
}

pub fn session_id_from(context: Option<&ExtensionContext>) -> Option<&str> {
    context
        .map(|context| context.session_manager.session_id())
        .filter(|id| !id.is_empty())
}

pub fn branch_entry_count(manager: Option<&dyn SessionManager>) -> usize {
    manager.map_or(0, |manager| manager.get_entries().len())
}

pub fn read_ui(context: Option<&ExtensionContext>) -> Option<&dyn ExtensionUi> {
    context.map(|context| context.ui.as_ref())
}

pub fn read_status_ui(context: Option<&ExtensionContext>) -> Option<StatusHandle> {
    context.map(|context| StatusHandle::from_extension_ui(Arc::clone(&context.ui)))
}

pub fn guarded_status(
    handle: &StatusHandle,
    allow: impl Fn() -> bool + Send + Sync + 'static,
) -> StatusHandle {
    let inner_allow = Arc::clone(&handle.allow);
    StatusHandle {
        host: Arc::clone(&handle.host),
        deliver: Arc::clone(&handle.deliver),
        allow: Arc::new(move || allow() && inner_allow()),
        request: None,
    }
}

pub fn sequenced_status(handle: &StatusHandle) -> StatusHandle {
    let mut state = handle.host.lock().unwrap_or_else(PoisonError::into_inner);
    state.requests += 1;
    let request = state.requests;
    drop(state);
    StatusHandle {
        host: Arc::clone(&handle.host),
        deliver: Arc::clone(&handle.deliver),
        allow: Arc::clone(&handle.allow),
        request: Some(request),
    }
}

pub fn is_record(value: &serde_json::Value) -> bool {
    value.is_object()
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Session;
    impl maho_ext_api::ToolSessionManager for Session {
        fn session_id(&self) -> &str {
            "session"
        }
        fn session_file(&self) -> Option<&std::path::Path> {
            None
        }
    }
    impl SessionManager for Session {
        fn get_entries(&self) -> Vec<maho_ext_api::SessionEntry> {
            vec![maho_ext_api::SessionEntry {
                id: "entry".into(),
                parent_id: None,
                timestamp: "now".into(),
                kind: "user".into(),
                data: maho_ext_api::JsonValue::Null,
            }]
        }
        fn get_branch(&self) -> Vec<maho_ext_api::SessionEntry> {
            vec![]
        }
        fn get_leaf_id(&self) -> Option<String> {
            None
        }
        fn get_session_name(&self) -> Option<String> {
            None
        }
    }
    #[test]
    fn absent_context_is_noop() {
        assert!(session_id_from(None).is_none());
        assert!(read_ui(None).is_none());
        assert_eq!(branch_entry_count(None), 0);
    }
    #[test]
    fn branch_count_uses_all_entries_not_current_branch() {
        assert_eq!(branch_entry_count(Some(&Session)), 1);
    }
    #[test]
    fn record_rejects_arrays_null_and_primitives() {
        assert!(is_record(&serde_json::json!({})));
        for value in [
            serde_json::Value::Null,
            serde_json::json!([]),
            serde_json::json!("text"),
            serde_json::json!(1),
        ] {
            assert!(!is_record(&value));
        }
    }

    #[derive(Default)]
    struct RecordingUi {
        calls: Mutex<Vec<(String, Option<String>)>>,
    }
    impl ExtensionUi for RecordingUi {
        fn select<'a>(
            &'a self,
            _: &'a str,
            _: &'a [String],
            _: maho_ext_api::ExtensionUiDialogOptions,
        ) -> maho_ext_api::UiFuture<'a, Option<String>> {
            Box::pin(async { None })
        }
        fn confirm<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
            _: maho_ext_api::ExtensionUiDialogOptions,
        ) -> maho_ext_api::UiFuture<'a, bool> {
            Box::pin(async { false })
        }
        fn input<'a>(
            &'a self,
            _: &'a str,
            _: Option<&'a str>,
            _: maho_ext_api::ExtensionUiDialogOptions,
        ) -> maho_ext_api::UiFuture<'a, Option<String>> {
            Box::pin(async { None })
        }
        fn notify(&self, _: &str, _: maho_ext_api::NotificationType) {}
        fn set_status(&self, key: &str, text: Option<&str>) {
            self.calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((key.to_owned(), text.map(str::to_owned)));
        }
        fn set_widget(
            &self,
            _: &str,
            _: Option<maho_ext_api::WidgetContent>,
            _: maho_ext_api::ExtensionWidgetOptions,
        ) {
        }
        fn set_header(&self, _: Option<maho_ext_api::ComponentFactory>) {}
        fn set_footer(&self, _: Option<maho_ext_api::ComponentFactory>) {}
        fn set_title(&self, _: &str) {}
        fn paste_to_editor(&self, _: &str) {}
        fn set_editor_text(&self, _: &str) {}
        fn get_editor_text(&self) -> String {
            String::new()
        }
        fn custom(
            &self,
            _: maho_ext_api::ComponentFactory,
            _: maho_ext_api::CustomUiOptions,
        ) -> maho_ext_api::ExtensionFuture<'_, serde_json::Value> {
            Box::pin(async { Ok(serde_json::Value::Null) })
        }
        fn theme(&self) -> maho_ext_api::Theme {
            maho_ext_api::Theme::default()
        }
    }

    #[test]
    fn shared_host_deduplicates_unchanged_lines_and_keeps_every_clear() {
        let host: Arc<RecordingUi> = Arc::default();
        let ui_arc: Arc<dyn ExtensionUi> = host.clone();
        let first = StatusHandle::from_extension_ui(Arc::clone(&ui_arc));
        let second = StatusHandle::from_extension_ui(ui_arc);

        first.set_status("memory", None);
        first.set_status("memory", Some("mem:project just now"));
        second.set_status("memory", Some("mem:project just now"));
        first.set_status("memory", Some("mem:project just now"));
        second.set_status("memory", Some("mem:project just now (+1)"));
        first.set_status("memory", None);
        second.set_status("memory", Some("mem:project just now"));

        let calls = host.calls.lock().unwrap_or_else(PoisonError::into_inner);
        let texts: Vec<Option<&str>> = calls.iter().map(|(_, text)| text.as_deref()).collect();
        assert_eq!(
            texts,
            vec![
                None,
                Some("mem:project just now"),
                Some("mem:project just now (+1)"),
                None,
                Some("mem:project just now"),
            ]
        );
    }

    #[test]
    fn status_keys_deduplicate_independently() {
        let host: Arc<RecordingUi> = Arc::default();
        let ui = StatusHandle::from_extension_ui(host.clone());
        ui.set_status("memory", Some("a"));
        ui.set_status("loop", Some("a"));
        ui.set_status("memory", Some("a"));

        let calls = host.calls.lock().unwrap_or_else(PoisonError::into_inner);
        assert_eq!(
            *calls,
            vec![
                ("memory".to_owned(), Some("a".to_owned())),
                ("loop".to_owned(), Some("a".to_owned())),
            ]
        );
    }

    #[test]
    fn failed_status_publish_is_not_remembered_so_retry_succeeds() {
        let fail_next = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let published: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
        let ui = StatusHandle::from_fallible({
            let fail_next = Arc::clone(&fail_next);
            let published = Arc::clone(&published);
            Arc::new(move |_key, text| {
                if fail_next.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    return Err("host unavailable".into());
                }
                published
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(text.map(str::to_owned));
                Ok(())
            })
        });

        assert!(
            ui.try_set_status("memory", Some("mem:project just now"))
                .is_err()
        );
        ui.try_set_status("memory", Some("mem:project just now"))
            .expect("retry after failed host publish");

        assert_eq!(
            *published.lock().unwrap_or_else(PoisonError::into_inner),
            vec![Some("mem:project just now".to_owned())]
        );
    }

    #[test]
    fn guarded_refusal_leaves_host_state_untouched_and_clear_supersedes_pre_clear_requests() {
        let host: Arc<RecordingUi> = Arc::default();
        let ui = StatusHandle::from_extension_ui(host.clone());
        sequenced_status(&ui).set_status("memory", Some("OLD"));
        let allowed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let refused = {
            let allowed = Arc::clone(&allowed);
            sequenced_status(&guarded_status(&ui, move || {
                allowed.load(std::sync::atomic::Ordering::SeqCst)
            }))
        };
        refused.set_status("memory", Some("NEW"));
        allowed.store(true, std::sync::atomic::Ordering::SeqCst);
        sequenced_status(&ui).set_status("memory", Some("NEW"));

        let before_clear = sequenced_status(&ui);
        ui.set_status("memory", None);
        ui.set_status("memory", Some("mem:a ⠋ reflecting"));
        before_clear.set_status("memory", Some("mem:a just now"));

        let calls = host.calls.lock().unwrap_or_else(PoisonError::into_inner);
        assert_eq!(
            *calls,
            vec![
                ("memory".to_owned(), Some("OLD".to_owned())),
                ("memory".to_owned(), Some("NEW".to_owned())),
                ("memory".to_owned(), None),
                ("memory".to_owned(), Some("mem:a ⠋ reflecting".to_owned())),
            ]
        );
    }
}
