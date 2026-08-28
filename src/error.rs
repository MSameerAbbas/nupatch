//! Error type for nupatch, surfaced through `miette` diagnostics.

use miette::Diagnostic;
use thiserror::Error;

/// All fatal error conditions. Rendered by `miette` with a code and help text.
#[derive(Debug, Error, Diagnostic)]
pub enum NupatchError {
    #[error("Could not find Cursor installation")]
    #[diagnostic(
        code(nupatch::cursor_not_found),
        help("Is Cursor installed? Expected under %LOCALAPPDATA%\\Programs\\cursor.")
    )]
    CursorNotFound,

    #[error("Could not find product.json")]
    #[diagnostic(
        code(nupatch::product_json_not_found),
        help("The Cursor installation looks incomplete or has an unexpected layout.")
    )]
    ProductJsonNotFound,

    #[error("Some patches failed")]
    #[diagnostic(
        code(nupatch::patch_failed),
        help("See the step output above for the failing patch.")
    )]
    PatchFailed,

    #[error("Checksum mismatch found")]
    #[diagnostic(
        code(nupatch::checksum_mismatch),
        help("Run `nupatch fix-checksums` to recompute product.json checksums.")
    )]
    ChecksumMismatch,

    #[error(transparent)]
    #[diagnostic(code(nupatch::io))]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    #[diagnostic(code(nupatch::json))]
    Json(#[from] serde_json::Error),
}

/// Convenience alias for fallible nupatch operations.
pub type Result<T> = std::result::Result<T, NupatchError>;
