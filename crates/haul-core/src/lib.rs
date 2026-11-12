//! Core of Haul: the download engine (queue, mirror grouping, scheduler,
//! resumable transfers), the per-host resolvers that turn page links into
//! direct links, and container import.
//!
//! The web server and the desktop app only present it, so everything that
//! talks to a host or touches the queue lives here.

pub mod bridge;
pub mod config;
pub mod container;
pub mod debug;
pub mod engine;
pub mod extract;
pub mod hosts;
mod item;
mod limiter;
pub mod logging;
mod names;
pub mod parts;
pub mod pass_ends;
pub mod paths;
pub mod router;
pub mod stay;
mod store;
pub mod throttle;
pub mod update;
pub mod vpn;

pub use config::{Config, Route, Theme};
pub use engine::{
    Activity, Added, Ask, Batch, BatchFailure, Browsing, Command, Engine, Event, Friction,
    HostView, ItemAction, ItemView, Options, PassPlan, RowState, SettingError, SettingsView,
    Burst, Snapshot, StartError, Telling, Told, WrapperAction, WrapperState, WrapperView, gather,
    summarize, summarize_burst,
};
pub use item::{Extract, Found, Verifying};

/// Locks that a panic elsewhere does not poison: what a panicking thread
/// left is used as it is, as everywhere in Haul.
pub mod lock {
    use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

    pub fn held<T: ?Sized>(m: &Mutex<T>) -> MutexGuard<'_, T> {
        m.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn read<T: ?Sized>(l: &RwLock<T>) -> RwLockReadGuard<'_, T> {
        l.read().unwrap_or_else(|e| e.into_inner())
    }

    pub fn write<T: ?Sized>(l: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
        l.write().unwrap_or_else(|e| e.into_inner())
    }
}
