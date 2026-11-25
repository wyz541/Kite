use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::agent::model::{AgentSessionEvent, AgentSessionEventCallback};

pub(crate) struct EventDispatcher {
    next_id: AtomicU64,
    listeners: RwLock<BTreeMap<u64, Arc<dyn AgentSessionEventCallback>>>,
}

impl EventDispatcher {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            next_id: AtomicU64::new(1),
            listeners: RwLock::new(BTreeMap::new()),
        })
    }

    pub(crate) fn subscribe(
        self: &Arc<Self>,
        callback: Box<dyn AgentSessionEventCallback>,
    ) -> Arc<AgentSessionEventSubscription> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let callback: Arc<dyn AgentSessionEventCallback> = Arc::from(callback);
        if let Ok(mut listeners) = self.listeners.write() {
            listeners.insert(id, callback);
        }
        Arc::new(AgentSessionEventSubscription {
            dispatcher: self.clone(),
            id,
            active: AtomicBool::new(true),
        })
    }

    pub(crate) fn emit(&self, event: AgentSessionEvent) {
        let listeners = self
            .listeners
            .read()
            .map(|listeners| listeners.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for listener in listeners {
            let event = event.clone();
            let _ = catch_unwind(AssertUnwindSafe(|| listener.on_event(event)));
        }
    }

    fn unsubscribe(&self, id: u64) {
        if let Ok(mut listeners) = self.listeners.write() {
            listeners.remove(&id);
        }
    }
}

#[derive(uniffi::Object)]
pub struct AgentSessionEventSubscription {
    dispatcher: Arc<EventDispatcher>,
    id: u64,
    active: AtomicBool,
}

#[uniffi::export]
impl AgentSessionEventSubscription {
    pub fn unsubscribe(&self) {
        if self.active.swap(false, Ordering::AcqRel) {
            self.dispatcher.unsubscribe(self.id);
        }
    }
}

impl Drop for AgentSessionEventSubscription {
    fn drop(&mut self) {
        self.unsubscribe();
    }
}
