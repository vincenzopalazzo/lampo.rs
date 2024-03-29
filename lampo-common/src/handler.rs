use crate::chan;
use crate::event::Event;

pub trait Handler: Send + Sync {
    fn events(&self) -> chan::Receiver<Event>;
    fn emit(&self, event: Event);
}
