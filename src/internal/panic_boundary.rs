// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
/// Runs user code while preventing a panicking panic-payload destructor from
/// escaping a callback boundary.
///
/// A payload whose destructor panics during unwinding is intentionally leaked
/// to avoid a second unwind. This cannot recover from process aborts, aborting
/// panic strategies, or callbacks/destructors that block forever.
///
/// # Parameters
///
/// * `operation` - User operation whose panic must be contained.
///
/// # Type Parameters
///
/// * `F` - Operation type.
/// * `T` - Operation result type.
pub(crate) fn contain_panic<F, T>(operation: F) -> Result<T, ()>
where
    F: FnOnce() -> T,
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation)) {
        Ok(value) => Ok(value),
        Err(payload) => {
            if let Err(nested) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(payload))) {
                std::mem::forget(nested);
            }
            Err(())
        }
    }
}
