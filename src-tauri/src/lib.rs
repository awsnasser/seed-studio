//! Seed Studio core. No Tauri types here: the desktop shell (main.rs) and the
//! dev bridge (bin/devserver.rs) both call `api::dispatch`.
pub mod agent;
pub mod api;
pub mod apilog;
pub mod bundle;
pub mod catalog;
pub mod cloud;
pub mod core;
pub mod db;
pub mod elements;
pub mod engine;
pub mod error;
pub mod graph;
pub mod library;
pub mod look;
pub mod provider;
pub mod secrets;
pub mod voice;
pub mod video;

pub use crate::core::Core;
