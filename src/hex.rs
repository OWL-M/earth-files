// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! Lowercase hex for digest output.
//!
//! The `sha2` and `md-5` 0.11 releases return `hybrid_array::Array`, which lacks the
//! `LowerHex` implementation of 0.10's `GenericArray`, so `format!("{digest:x}")` no
//! longer compiles. Both call sites need the same output, and one requires it for
//! compatibility.

use std::fmt::Write as _;

/// Lowercase, zero-padded hex with two characters per byte, matching `{:x}` on the old
/// `GenericArray` and the output of `md5sum`/`sha256sum`.
pub fn lower(bytes: impl AsRef<[u8]>) -> String {
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::lower;

    #[test]
    fn pads_each_byte_to_two_lowercase_digits() {
        assert_eq!(lower([0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
        assert_eq!(lower([]), "");
    }
}
