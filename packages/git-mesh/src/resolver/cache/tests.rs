//! Cache contract tests.
//!
//! Phase 1 places the legacy sqlite-era tests behind `#[cfg(any())]` so they
//! compile-out while the API is being rebuilt. Phase 2 retargets them at the
//! new [`Cache`] / [`Kind`] / [`CacheKey`] surface as `#[ignore]` skipped
//! contract checks; Phase 3 unskips them tier by tier.

#[cfg(any())]
mod legacy_sqlite_tests {
    use super::*;
}
