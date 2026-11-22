use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

pub struct SessionWorkerDataGrant {
    pub value: Value,
    pub dispose: Option<Box<dyn FnOnce() + Send>>,
}

pub type SessionWorkerDataProvider =
    Arc<dyn Fn() -> Result<SessionWorkerDataGrant, String> + Send + Sync>;

pub struct IssuedSessionWorkerData {
    pub extension_data: BTreeMap<String, Value>,
    disposed: AtomicBool,
    disposers: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
}

impl IssuedSessionWorkerData {
    pub fn dispose(&self) {
        if self.disposed.swap(true, Ordering::SeqCst) {
            return;
        }
        let disposers = std::mem::take(
            &mut *self
                .disposers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for dispose in disposers {
            dispose();
        }
    }
}

#[derive(Default)]
struct RegistryState {
    providers: BTreeMap<String, SessionWorkerDataProvider>,
    sealed: bool,
}

#[derive(Default, Clone)]
pub struct SessionWorkerDataRegistry {
    state: Arc<Mutex<RegistryState>>,
}

impl SessionWorkerDataRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn provide(
        &self,
        name: impl Into<String>,
        create: SessionWorkerDataProvider,
    ) -> Result<(), String> {
        let name = name.into();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.sealed {
            return Err(format!(
                "session worker data \"{name}\" was provided after the first session worker"
            ));
        }
        if state.providers.contains_key(&name) {
            return Err(format!(
                "session worker data \"{name}\" is already provided"
            ));
        }
        state.providers.insert(name, create);
        Ok(())
    }

    pub fn spawn<T>(
        &self,
        start: impl FnOnce(&IssuedSessionWorkerData) -> Result<T, String>,
    ) -> Result<(T, Arc<IssuedSessionWorkerData>), String> {
        let providers: Vec<(String, SessionWorkerDataProvider)> = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.sealed = true;
            state
                .providers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        };
        let mut extension_data = BTreeMap::new();
        let mut disposers = Vec::new();
        for (name, create) in providers {
            match create() {
                Ok(grant) => {
                    extension_data.insert(name, grant.value);
                    if let Some(dispose) = grant.dispose {
                        disposers.push(dispose);
                    }
                }
                Err(error) => {
                    for dispose in disposers {
                        dispose();
                    }
                    return Err(error);
                }
            }
        }
        let issued = Arc::new(IssuedSessionWorkerData {
            extension_data,
            disposed: AtomicBool::new(false),
            disposers: Mutex::new(disposers),
        });
        match start(&issued) {
            Ok(result) => Ok((result, issued)),
            Err(error) => {
                issued.dispose();
                Err(error)
            }
        }
    }
}

static GLOBAL_REGISTRY: LazyLock<SessionWorkerDataRegistry> =
    LazyLock::new(SessionWorkerDataRegistry::new);

pub fn session_worker_data_registry() -> &'static SessionWorkerDataRegistry {
    &GLOBAL_REGISTRY
}

pub fn provide_session_worker_data(
    name: impl Into<String>,
    create: SessionWorkerDataProvider,
) -> Result<(), String> {
    GLOBAL_REGISTRY.provide(name, create)
}

pub fn read_session_worker_data(data: &Value, name: &str) -> Option<Value> {
    data.as_object()?
        .get("extensionData")?
        .as_object()?
        .get(name)
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn issues_distinct_values_per_worker_seals_after_first_spawn_and_disposes_once() {
        let registry = SessionWorkerDataRegistry::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let disposed = Arc::new(AtomicUsize::new(0));
        let counter_for_provider = counter.clone();
        let disposed_for_provider = disposed.clone();
        registry
            .provide(
                "token",
                Arc::new(move || {
                    let next = counter_for_provider.fetch_add(1, Ordering::SeqCst) + 1;
                    let disposed = disposed_for_provider.clone();
                    Ok(SessionWorkerDataGrant {
                        value: json!(next),
                        dispose: Some(Box::new(move || {
                            disposed.fetch_add(1, Ordering::SeqCst);
                        })),
                    })
                }),
            )
            .expect("provide");

        assert!(
            registry
                .provide(
                    "token",
                    Arc::new(|| Ok(SessionWorkerDataGrant {
                        value: Value::Null,
                        dispose: None,
                    }))
                )
                .is_err()
        );

        let (first_val, first_issued) = registry
            .spawn(|issued| Ok(issued.extension_data["token"].clone()))
            .expect("first spawn");
        let (second_val, second_issued) = registry
            .spawn(|issued| Ok(issued.extension_data["token"].clone()))
            .expect("second spawn");
        assert_eq!(first_val, json!(1));
        assert_eq!(second_val, json!(2));

        assert!(
            registry
                .provide(
                    "late",
                    Arc::new(|| Ok(SessionWorkerDataGrant {
                        value: Value::Null,
                        dispose: None,
                    }))
                )
                .is_err()
        );

        first_issued.dispose();
        first_issued.dispose();
        assert_eq!(disposed.load(Ordering::SeqCst), 1);
        second_issued.dispose();
        assert_eq!(disposed.load(Ordering::SeqCst), 2);
    }
}
