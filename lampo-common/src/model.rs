mod connect;
mod getinfo;
mod new_addr;
mod on_chain;
mod open_channel;

pub use connect::Connect;
pub use getinfo::GetInfo;

pub mod request {
    pub use crate::model::connect::Connect;
    pub use crate::model::getinfo::GetInfo;
    pub use crate::model::new_addr::request::*;
    pub use crate::model::on_chain::request::*;
    pub use crate::model::open_channel::request::*;
}

pub mod response {
    pub use crate::model::connect::Connect;
    pub use crate::model::getinfo::GetInfo;
    pub use crate::model::new_addr::response::*;
    pub use crate::model::on_chain::response::*;
    pub use crate::model::open_channel::response::*;
}
