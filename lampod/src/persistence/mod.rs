//! Persistence module implementation for lampo
//!
//! N.B: This is an experimental version of the persistence,
//! please do not use it in production you can lost funds, or
//! in others words you WILL lost funds, do not trush me!
use std::sync::Arc;

use bitcoin::OutPoint;
use lightning::chain::{
    chainmonitor::Persist,
    channelmonitor::{ChannelMonitor, ChannelMonitorUpdate},
};
use lightning_persister::FilesystemPersister;

/// Lampo Persistence implementation.
// FIME: it is a simple wrapper around the ldk file persister
// giving more time to understand how to make a custom one without
// lost funds :-P
pub type LampoPersistence = FilesystemPersister;
