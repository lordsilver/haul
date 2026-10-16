//! Core of Haul: the download engine (queue, mirror grouping, scheduler,
//! resumable transfers), the per-host resolvers that turn page links into
//! direct links, and container import.
//!
//! The web server and the desktop app only present it, so everything that
//! talks to a host or touches the queue lives here.

pub mod bridge;
pub mod config;
pub mod container;
pub mod engine;
pub mod hosts;
mod item;
mod limiter;
pub mod paths;
pub mod stay;
mod store;

pub use config::{Config, Route, Theme};
pub use engine::{
    Added, Engine, Event, HostView, ItemAction, ItemView, Options, SettingsView, Snapshot,
    StartError, WrapperView,
};
pub use item::{Found, Verifying};
