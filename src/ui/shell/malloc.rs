// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! glibc allocator tuning.
//!
//! Ported from pop-os/libcosmic d9431dc, src/malloc.rs.

/// Returns free memory at the top of the heap to the OS.
#[inline]
pub fn trim(pad: usize) {
    unsafe {
        libc::malloc_trim(pad);
    }
}

/// Prevents glibc from hoarding memory via memory fragmentation.
#[inline]
pub fn limit_mmap_threshold(threshold: i32) {
    unsafe {
        libc::mallopt(libc::M_MMAP_THRESHOLD, threshold);
    }
}
