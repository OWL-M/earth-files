// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! glibc allocator tuning.
//!
//! Ported from pop-os/libcosmic d9431dc, src/malloc.rs.

use std::os::raw::c_int;

const M_MMAP_THRESHOLD: c_int = -3;

unsafe extern "C" {
    fn malloc_trim(pad: usize);

    fn mallopt(param: c_int, value: c_int) -> c_int;
}

/// Returns free memory at the top of the heap to the OS.
#[inline]
pub fn trim(pad: usize) {
    unsafe {
        malloc_trim(pad);
    }
}

/// Prevents glibc from hoarding memory via memory fragmentation.
#[inline]
pub fn limit_mmap_threshold(threshold: i32) {
    unsafe {
        mallopt(M_MMAP_THRESHOLD, threshold);
    }
}
