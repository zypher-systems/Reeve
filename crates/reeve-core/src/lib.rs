//! Reeve core: configuration, secrets, model providers, and spend.
//!
//! See `design.md` at the repository root for the whole picture.

#![forbid(unsafe_code)]

pub mod agent;
pub mod config;
pub mod diff;
pub mod error;
pub mod ledger;
pub mod llm;
pub mod policy;
pub mod receipts;
pub mod session;
pub mod settings;
pub mod spend;
pub mod tools;
pub mod undo;

pub use error::{Error, Result};
