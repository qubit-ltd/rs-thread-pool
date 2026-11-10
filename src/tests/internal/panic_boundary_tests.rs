// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================

use crate::internal::contain_panic;

struct Payload;

impl Drop for Payload {
    fn drop(&mut self) {
        panic!("payload destructor");
    }
}

#[test]
fn test_panicking_payload_destructor_is_contained() {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        contain_panic(|| std::panic::panic_any(Payload))
    }));
    assert!(outcome.is_ok(), "nested payload destructor panic must not escape");
    assert!(outcome.expect("outer boundary should return").is_err());
}
