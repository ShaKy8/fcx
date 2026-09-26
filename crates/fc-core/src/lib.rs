//! Core logic for fc. This crate must never depend on GTK so it stays unit-testable.

pub mod action;
pub mod archive;
pub mod checksum;
pub mod compare;
pub mod config;
pub mod diff;
pub mod favorites;
pub mod format;
pub mod fs;
pub mod glob;
pub mod jobs;
pub mod keymap;
pub mod rename;
pub mod search;
pub mod sort;
pub mod text;
pub mod users;
