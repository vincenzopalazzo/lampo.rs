//! Events commands
pub mod ln;
pub mod onchain;

use std::sync::{Arc, Mutex};

use crate::chan;
use crate::event::ln::LightningEvent;
use crate::event::onchain::OnChainEvent;

/// Publishes events to subscribers.
#[derive(Clone)]
pub struct Emitter<T> {
    subscribers: Arc<Mutex<Vec<chan::UnboundedSender<T>>>>,
}

impl<T> Default for Emitter<T> {
    fn default() -> Self {
        Self {
            subscribers: Default::default(),
        }
    }
}

impl<T: Clone> Emitter<T> {
    /// Emit an event to all subscribers and drop subscribers who can't receive it.
    pub fn emit(&self, event: T) {
        self.subscribers
            .lock()
            .unwrap()
            .retain(|s| s.send(event.clone()).is_ok());
    }

    /// Drop all subscribers.
    pub fn close(self) {
        self.subscribers.lock().unwrap().clear();
    }

    /// Create a subscriber from this emitter.
    pub fn subscriber(&self) -> Subscriber<T> {
        Subscriber {
            subscribers: self.subscribers.clone(),
        }
    }
}

/// Subscribes to events.
#[derive(Clone)]
pub struct Subscriber<T> {
    subscribers: Arc<Mutex<Vec<chan::UnboundedSender<T>>>>,
}

impl<T: Clone> Subscriber<T> {
    /// Add a subscription to receive broadcast events.
    pub fn subscribe(&self) -> chan::UnboundedReceiver<T> {
        let (sender, receiver) = chan::unbounded_channel::<T>();
        let mut subs = self.subscribers.lock().unwrap();
        subs.push(sender);
        receiver
    }
}

#[derive(Debug, Clone)]
pub enum Event {
    Lightning(LightningEvent),
    OnChain(OnChainEvent),
    Inventory,
}
