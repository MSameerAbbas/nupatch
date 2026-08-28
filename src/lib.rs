//! nupatch: patch Cursor's CLI and IDE agents to use nushell instead of
//! PowerShell on Windows.
//!
//! The crate is split into focused modules: [`discovery`] recovers minified
//! names, [`patch`] applies the transforms, [`integrity`] maintains Cursor's
//! hash chain, [`paths`] locates the install, and [`ui`]/[`cli`] render output.

pub mod cli;
pub mod discovery;
pub mod error;
pub mod integrity;
pub mod patch;
pub mod paths;
pub mod ui;
pub mod util;
