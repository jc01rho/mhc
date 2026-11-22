use crate::extension_owner::{ExtensionOwner, ExtensionOwnerIdentity, ExtensionTransportOwner};
use std::{cell::RefCell, sync::Arc};

#[derive(Clone)]
struct CapturedConnection {
    id: String,
    owner: Option<ExtensionTransportOwner>,
    binding: Option<ExtensionOwner>,
}

thread_local! {
    static CURRENT_CAPTURE: RefCell<Option<CapturedConnection>> = const { RefCell::new(None) };
}

pub type TransportOwnerLookup = Arc<dyn Fn(&str) -> Option<ExtensionTransportOwner> + Send + Sync>;

#[derive(Clone)]
pub struct SessionConnectionContext {
    owner_of: TransportOwnerLookup,
}

impl SessionConnectionContext {
    pub fn new(owner_of: TransportOwnerLookup) -> Self {
        Self { owner_of }
    }

    fn with_store<T>(next: Option<CapturedConnection>, task: impl FnOnce() -> T) -> T {
        struct Guard(Option<CapturedConnection>);
        impl Drop for Guard {
            fn drop(&mut self) {
                let prev = self.0.take();
                CURRENT_CAPTURE.with(|cell| *cell.borrow_mut() = prev);
            }
        }
        let prev = CURRENT_CAPTURE.with(|cell| {
            let mut borrowed = cell.borrow_mut();
            std::mem::replace(&mut *borrowed, next)
        });
        let _guard = Guard(prev);
        task()
    }

    pub fn run<T>(&self, id: &str, task: impl FnOnce() -> T) -> T {
        let owner = (self.owner_of)(id);
        Self::with_store(
            Some(CapturedConnection {
                id: id.to_owned(),
                owner,
                binding: None,
            }),
            task,
        )
    }

    pub fn run_captured<T>(
        &self,
        id: &str,
        identity: &ExtensionOwnerIdentity,
        task: impl FnOnce() -> T,
    ) -> T {
        let owner = (self.owner_of)(id);
        let binding = owner
            .as_ref()
            .filter(|owner| owner.owner_id() == identity.owner_id)
            .and_then(|owner| owner.peek(&identity.binding_generation));
        Self::with_store(
            Some(CapturedConnection {
                id: id.to_owned(),
                owner: binding.as_ref().and(owner),
                binding,
            }),
            task,
        )
    }

    pub fn run_issued<T>(&self, id: &str, binding: ExtensionOwner, task: impl FnOnce() -> T) -> T {
        Self::with_store(
            Some(CapturedConnection {
                id: id.to_owned(),
                owner: None,
                binding: Some(binding),
            }),
            task,
        )
    }

    pub fn outside<T>(&self, task: impl FnOnce() -> T) -> T {
        Self::with_store(None, task)
    }

    pub fn get_store(&self) -> Option<String> {
        CURRENT_CAPTURE.with(|cell| cell.borrow().as_ref().map(|c| c.id.clone()))
    }

    pub fn transport_of(&self, id: &str) -> Option<ExtensionTransportOwner> {
        (self.owner_of)(id)
    }

    pub fn transport(&self) -> Option<ExtensionTransportOwner> {
        CURRENT_CAPTURE.with(|cell| cell.borrow().as_ref().and_then(|c| c.owner.clone()))
    }

    pub fn owner(&self) -> Option<ExtensionOwner> {
        CURRENT_CAPTURE.with(|cell| cell.borrow().as_ref().and_then(|c| c.binding.clone()))
    }

    pub fn with_binding<T>(&self, generation: &str, task: impl FnOnce() -> T) -> T {
        let captured = CURRENT_CAPTURE.with(|cell| cell.borrow().clone());
        let Some(mut captured) = captured else {
            return task();
        };
        let Some(owner) = &captured.owner else {
            return task();
        };
        if let Ok(binding) = owner.capture(generation) {
            captured.binding = Some(binding);
        }
        Self::with_store(Some(captured), task)
    }

    pub fn is_current(&self) -> bool {
        (self.current_guard())()
    }

    pub fn current_guard(&self) -> Arc<dyn Fn() -> bool + Send + Sync> {
        let captured = CURRENT_CAPTURE.with(|cell| cell.borrow().clone());
        let Some(captured) = captured else {
            return Arc::new(|| true);
        };
        let owner_of = self.owner_of.clone();
        Arc::new(move || {
            let Some(captured_owner) = &captured.owner else {
                return false;
            };
            let current = (owner_of)(&captured.id);
            current
                .as_ref()
                .is_some_and(|cur| cur.owner_id() == captured_owner.owner_id())
                && !captured_owner.retired()
                && !captured
                    .binding
                    .as_ref()
                    .is_some_and(ExtensionOwner::retired)
        })
    }
}
