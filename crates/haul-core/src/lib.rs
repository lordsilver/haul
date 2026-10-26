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
pub mod parts;
pub mod paths;
pub mod router;
pub mod stay;
mod store;
pub mod update;
pub mod vpn;

pub use config::{Config, Route, Theme};
pub use engine::{
    Added, Command, Engine, Event, HostView, ItemAction, ItemView, Options, SettingsView, Snapshot,
    StartError, WrapperView,
};
pub use item::{Extract, Found, Verifying};
