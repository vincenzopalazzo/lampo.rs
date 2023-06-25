use crate::chan;
use crate::event::Event;

pub trait Handler {
    fn events(&self) -> chan::Receiver<Event>;
    fn emit(&self, event: Event);
}
