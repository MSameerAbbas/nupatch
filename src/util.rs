//! Shared utilities.

use fancy_regex::{Regex, RegexBuilder};

/// Build a Regex with a generous backtrack limit (fallible).
/// Use for dynamic patterns that interpolate runtime variables.
/// Cursor's minified JS files are large enough to exceed fancy_regex's
/// default 1M limit on patterns with look-around.
pub fn re(pattern: &str) -> Result<Regex, fancy_regex::Error> {
    RegexBuilder::new(pattern)
        .backtrack_limit(10_000_000)
        .build()
}

/// Build a Regex with a generous backtrack limit (panics on invalid pattern).
/// Only for compile-time constant patterns used in `lazy_re!`.
#[doc(hidden)]
pub fn re_unchecked(pattern: &str) -> Regex {
    re(pattern).expect("invalid constant regex pattern")
}

/// Compile a regex once and cache it in a `LazyLock` static.
/// Use for constant patterns only -- dynamic patterns that interpolate
/// variables at runtime should keep using `re()`.
macro_rules! lazy_re {
    ($pattern:expr) => {{
        static RE: std::sync::LazyLock<fancy_regex::Regex> =
            std::sync::LazyLock::new(|| $crate::util::re_unchecked($pattern));
        &*RE
    }};
}
pub(crate) use lazy_re;