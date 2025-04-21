//! Events commands
pub mod ln;
pub mod onchain;

use crate::event::ln::LightningEvent;
use crate::event::onchain::OnChainEvent;
use std::sync::{Arc, LockResult, Mutex, MutexGuard};
use tokio::sync::mpsc;

/// Publishes events to subscribers.
#[derive(Clone)]
pub struct Emitter<T> {
    subscribers: Arc<Mutex<Vec<mpsc::UnboundedSender<T>>>>,
}

impl<T> Default for Emitter<T> {
    fn default() -> Self {
        Self {
            subscribers: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl<T: Clone + Send + 'static + std::fmt::Debug> Emitter<T> {
    /// Emit an event to all subscribers and drop subscribers who can't receive it.
    pub fn emit(&self, event: T) {
        let subs_guard: LockResult<MutexGuard<Vec<mpsc::UnboundedSender<T>>>> =
            self.subscribers.lock();

        let Ok(mut subs) = subs_guard else {
            log::error!("Event emitter mutex poisoned");
            return;
        };

        subs.retain(|sender| {
            match sender.send(event.clone()) {
                Ok(_) => true,
                Err(e) => {
                    // Receiver was dropped, log and remove sender
                    log::trace!("Receiver dropped, removing subscriber: {:?}", e);
                    false
                }
            }
        });
    }

    /// Drop all subscribers.
    pub fn close(self) {
        if let Ok(mut subs) = self.subscribers.lock() {
            subs.clear();
        } else {
            log::error!("Event emitter mutex poisoned during close!");
        }
    }

    /// Create a subscriber from this emitter.
    pub fn subscriber(&self) -> Subscriber<T> {
        Subscriber {
            subscribers: Arc::clone(&self.subscribers),
        }
    }
}

/// Subscribes to events.
#[derive(Clone)]
pub struct Subscriber<T> {
    subscribers: Arc<Mutex<Vec<mpsc::UnboundedSender<T>>>>,
}

impl<T: Clone + Send + 'static> Subscriber<T> {
    /// Add a subscription to receive broadcast events.
    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<T> {
        let (sender, receiver) = mpsc::unbounded_channel();
        if let Ok(mut subs) = self.subscribers.lock() {
            subs.push(sender);
        } else {
            log::error!("Event subscriber mutex poisoned!");
        }
        receiver
    }
}

#[derive(Debug, Clone)]
pub enum Event {
    Lightning(LightningEvent),
    OnChain(OnChainEvent),
    Inventory,
}
