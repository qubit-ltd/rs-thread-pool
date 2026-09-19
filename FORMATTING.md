# Code Formatting Guide

## Overview

This project uses **nightly Rust's `rustfmt`** for code formatting to support advanced formatting features, specifically the `imports_layout = "Vertical"` option.

## Why Nightly?

The `imports_layout` configuration option is an **unstable feature** that requires nightly Rust. This allows us to format imports with each item on its own line:

```rust
pub use module::{
    Item1,
    Item2,
    Item3,
};
```

**Official Documentation:**
- [Rustfmt Configuration Documentation](https://rust.googlesource.com/rustfmt/+/105cbf8ed517d9e42ae1dc837bdc97cc3ff28175/Configurations.md)
- [GitHub Tracking Issue #5083](https://github.com/rust-lang/rustfmt/issues/5083)

## How to Format Code

### Recommended command

```bash
./align-ci.sh
```

This script will:
The script uses the pinned `nightly-2026-06-05` toolchain and checks the
repository formatting configuration.

### Option 2: Manual Command

```bash
# Install nightly toolchain (if not already installed)
rustup toolchain install nightly-2026-06-05

# Format code
cargo +nightly-2026-06-05 fmt --all -- --check
```

## CI/CD Integration

The local checks use `./ci-check.sh`; style-only checks use `./style-check.sh`.

- **Local checks**: Run `./ci-check.sh` before release validation.
- **CI**: The repository workflow invokes the same project scripts.

## Configuration

The formatting configuration is defined in `.infra/style/rustfmt.toml`:

```toml
# Format imports with vertical layout (each item on its own line within braces)
imports_layout = "Vertical"
```

This is the **only configuration option** needed to achieve our desired formatting style.

## Important Notes

1. **Formatting uses the pinned nightly Rust toolchain**.
2. **Local development**: Can use either stable or nightly; formatting requires nightly
3. **Automatic installation**: The `ci-check.sh` script automatically installs nightly toolchain if needed
4. **No manual intervention**: `align-ci.sh` selects the configured toolchain.

## Troubleshooting

### Format check fails in CI

Make sure your code is formatted before committing:

```bash
./align-ci.sh
```

### Nightly toolchain issues

Reinstall the nightly toolchain:

```bash
rustup toolchain uninstall nightly
rustup toolchain install nightly --component rustfmt
```

## References

- [Rustfmt Documentation](https://rust-lang.github.io/rustfmt/)
- [Rustup Documentation](https://rust-lang.github.io/rustup/)
