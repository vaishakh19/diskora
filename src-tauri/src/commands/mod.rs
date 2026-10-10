//! Tauri command surface.
//!
//! Every command is narrowly scoped and does one thing. The frontend cannot run arbitrary code,
//! read arbitrary paths or delete anything without going through one of these, and each one
//! revalidates its input before acting.
//!
//! Naming: snake_case here, camelCase on the TypeScript side — Tauri converts automatically.

pub mod drives;
pub mod files;
pub mod large_files;
pub mod scan;
