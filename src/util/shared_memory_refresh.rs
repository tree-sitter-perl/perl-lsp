//! Allocator refresh for the threaded wasm build.
//!
//! V8 before 15.7 (Chromium 157) lets a thread keep a stale size for a shared
//! memory another thread grew, until that thread's next stack check. Plain
//! loads and stores are unaffected, but `memory.copy`, `memory.fill` and the
//! atomic operations are bounds-checked against the stale size and trap on an
//! in-bounds address. The window opens where a thread is handed memory it has
//! not seen grow, so the link wraps the allocator (`--wrap`, `build.rs`) and
//! each wrapper runs `memory.grow 0` — which makes the engine re-read the
//! size — when the block it returns ends past what this thread believes.
//!
//! `calloc` and `realloc` are spelled over `malloc` because libc's own clear
//! and copy run inside the call, ahead of any refresh a wrapper could make.

use core::arch::wasm32::{memory_grow, memory_size};
use core::ffi::c_void;

const PAGE: usize = 65536;

extern "C" {
    fn __real_malloc(size: usize) -> *mut c_void;
    fn __real_aligned_alloc(align: usize, size: usize) -> *mut c_void;
    fn __real_posix_memalign(out: *mut *mut c_void, align: usize, size: usize) -> i32;
    fn malloc_usable_size(block: *mut c_void) -> usize;
    fn free(block: *mut c_void);
}

#[inline]
fn refresh(block: *mut c_void, size: usize) -> *mut c_void {
    if !block.is_null() && (block as usize).saturating_add(size) > memory_size::<0>() * PAGE {
        memory_grow::<0>(0);
    }
    block
}

#[no_mangle]
unsafe extern "C" fn __wrap_malloc(size: usize) -> *mut c_void {
    refresh(__real_malloc(size), size)
}

#[no_mangle]
unsafe extern "C" fn __wrap_aligned_alloc(align: usize, size: usize) -> *mut c_void {
    refresh(__real_aligned_alloc(align, size), size)
}

#[no_mangle]
unsafe extern "C" fn __wrap_posix_memalign(
    out: *mut *mut c_void,
    align: usize,
    size: usize,
) -> i32 {
    let status = __real_posix_memalign(out, align, size);
    if status == 0 {
        refresh(*out, size);
    }
    status
}

#[no_mangle]
unsafe extern "C" fn __wrap_calloc(count: usize, each: usize) -> *mut c_void {
    let Some(size) = count.checked_mul(each) else {
        return core::ptr::null_mut();
    };
    let block = __wrap_malloc(size);
    if !block.is_null() {
        core::ptr::write_bytes(block.cast::<u8>(), 0, size);
    }
    block
}

#[no_mangle]
unsafe extern "C" fn __wrap_realloc(old: *mut c_void, size: usize) -> *mut c_void {
    if old.is_null() {
        return __wrap_malloc(size);
    }
    let block = __wrap_malloc(size);
    if !block.is_null() {
        let kept = malloc_usable_size(old).min(size);
        core::ptr::copy_nonoverlapping(old.cast::<u8>(), block.cast::<u8>(), kept);
        free(old);
    }
    block
}
