//! Persistence module implementation for lampo
//!
//! N.B: This is an experimental version of the persistence,
//! please do not use it in production you can lost funds, or
//! in others words you WILL lost funds, do not trush me!
use lampo_common::ldk::persister::fs_store::FilesystemStore;

/// Lampo Persistence implementation.
// FIME: it is a simple wrapper around the ldk file persister
// giving more time to understand how to make a custom one without
// lost funds :-P
pub type LampoPersistence = FilesystemStore;
