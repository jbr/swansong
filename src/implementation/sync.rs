//! The synchronization primitives that carry swansong's stop and guard protocols. Under
//! `--cfg swansong_loom` they are loom's, so that the models in `shard.rs` explore the real code.

#[cfg(swansong_loom)]
pub(crate) use loom::sync::{
    atomic::{AtomicU64, AtomicUsize},
    Mutex,
};
#[cfg(not(swansong_loom))]
pub(crate) use std::sync::{
    atomic::{AtomicU64, AtomicUsize},
    Mutex,
};
