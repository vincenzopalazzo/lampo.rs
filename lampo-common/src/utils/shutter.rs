use triggered::{Listener, Trigger};

#[derive(Clone)]
pub struct Shutter {
	trigger: Trigger,
	signal: Listener,
}

impl Shutter {
	pub fn new() -> Self {
		let (trigger, signal) = triggered::trigger();
		Self { trigger, signal }
	}

    pub fn signal(&self) -> Listener {
        self.signal.clone()
    }

    pub fn trigger(&self) -> Trigger {
        self.trigger.clone()
    }
}
