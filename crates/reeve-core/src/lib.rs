//! Reeve core: configuration, secrets, model providers, and spend.
//!
//! See `design.md` at the repository root for the whole picture.

#![forbid(unsafe_code)]

pub mod agent;
pub mod config;
pub mod diff;
pub mod distro;
pub mod error;
pub mod findings;
pub mod ledger;
pub mod llm;
pub mod memory;
pub mod orders;
pub mod pacman;
pub mod policy;
pub mod privacy;
pub mod receipts;
pub mod report;
pub mod root;
pub mod scratch;
pub mod session;
pub mod settings;
pub mod snapshots;
pub mod spend;
pub mod sudo;
pub mod tools;
pub mod txn;
pub mod undo;
pub mod update;

pub use error::{Error, Result};
