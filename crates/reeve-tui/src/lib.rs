//! Reeve's TUI: a board of tiles, the chat, and each tile opened.

#![forbid(unsafe_code)]

pub mod board;
pub mod cards;
pub mod draw;
pub mod ledger;
pub mod orderform;
pub mod overlay;
pub mod panels;
pub mod run;
pub mod screens;
#[cfg(test)]
mod shots;
pub mod theme;
pub mod view;

pub use run::run;
