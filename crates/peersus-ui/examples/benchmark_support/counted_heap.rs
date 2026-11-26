//! The allocator of a measuring program: the system's, with its bytes counted.
//!
//! A program that includes this module allocates exactly as a product program does and
//! additionally knows what its Rust heap holds, held at most and was ever asked for. The
//! product programs never include it. What C and C++ libraries allocate themselves (the
//! renderer, graphics drivers) does not pass through it and is not counted.

use cap::Cap;
use peersus_ui::probe::Heap;
use std::alloc::System;

#[global_allocator]
static COUNTED: Cap<System> = Cap::new(System, usize::MAX);

/// Give the measuring code the way to read this heap. Call it first thing in `main`.
pub fn install() {
    peersus_ui::probe::count_heap(|| Heap {
        live: COUNTED.allocated(),
        peak: COUNTED.max_allocated(),
        allocated: COUNTED.total_allocated(),
    });
}
