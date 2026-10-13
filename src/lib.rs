//! MacDirStat: a WinDirStat-style disk usage treemap for macOS.
//!
//! The library holds everything but the process entry point so the scanner
//! and renderer can be exercised by tests and examples.

pub mod access;
pub mod app;
pub mod bulk;
pub mod format;
pub mod icon;
pub mod scan;
pub mod theme;
pub mod tree;
pub mod treemap;
pub mod volumes;
pub mod worker;
