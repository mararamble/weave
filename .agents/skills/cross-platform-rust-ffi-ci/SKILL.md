---
name: cross-platform-rust-ffi-ci
description: Prevent and diagnose Rust FFI, lint, and CI discrepancies across supported operating systems. Use when calling libc/platform APIs, changing unsafe code, investigating CI-only failures, or validating cross-platform Rust changes.
---

# Cross-platform Rust FFI and CI

## Workflow

1. Read repository guidance (`AGENTS.md`), the affected code, dependency versions, and the workflow matrix before editing.
2. Identify every supported target and inspect the actual FFI declarations for those targets in the pinned dependency source or official API documentation. Do not infer signatures from one operating system.
3. Check pointer mutability, integer widths, struct layout, constants, feature gates, and safety preconditions for each target.
4. Express genuine ABI differences with narrowly scoped `#[cfg(target_os = "...")]` branches or small wrappers. Keep each branch type-correct for its platform. Prefer `&raw const` / `&raw mut` when an FFI function requires different pointer mutability; avoid unnecessary reference formation and avoid pointer casts that obscure the contract.
5. Document the reason for target-specific unsafe code near the call. Keep unsafe blocks as small as practical.
6. Never silence a lint that exposes a real signature difference with `allow`, broad lint exceptions, or a cast chosen only to make the code compile.
7. Confirm CI runs formatting, tests, and warnings-as-errors Clippy on every supported OS. Add a matrix entry when coverage is missing; avoid relying only on the developer's host platform.
8. Run the repository's documented checks locally. If cross-target execution is unavailable, compile/check the target if possible and leave runtime confirmation to the OS runner. Report exactly which checks ran and where.
9. Inspect the final diff, preserve pre-existing user edits, and do not commit or push unless requested.

## Validation checklist

From the repository root, use the project-specific equivalents of:

```sh
cargo fmt --check
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
```

Run these on each supported CI operating system. For FFI changes, specifically verify that each target's dependency declaration accepts the pointer type supplied by its cfg branch and that the test exercises the relevant behavior on those targets.

## Example: `libc::openpty`

In the current libc declarations, macOS declares `winp: *mut winsize`, while Linux declares `winp: *const winsize`. A direct mutable reference is unnecessary on Linux, and a shared reference is incompatible with macOS. An explicit target-specific raw pointer keeps both declarations correct:

```rust
#[cfg(target_os = "macos")]
let size_ptr = &raw mut size;
#[cfg(target_os = "linux")]
let size_ptr = &raw const size;
```

Pass `size_ptr` to the call after verifying the actual declarations in the project's resolved libc version. Extend supported targets deliberately rather than assuming their declarations match.
