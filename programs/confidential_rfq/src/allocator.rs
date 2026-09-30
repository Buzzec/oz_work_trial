//! Reclaim dropped FHE builders between CPI phases within Solana's default 32 KiB heap.
//! The allocator metadata lives in the invocation's heap because SBF globals are read-only.

use core::{
    alloc::{GlobalAlloc, Layout},
    mem::{MaybeUninit, size_of},
    ptr::{self, NonNull},
};
use linked_list_allocator::Heap;

#[repr(C)]
struct HeapState {
    initialized: usize,
    heap: MaybeUninit<Heap>,
}

/// Only used on a single-threaded SBF invocation (or an isolated single-threaded test).
struct ReclaimingAllocator {
    start: usize,
    len: usize,
}

impl ReclaimingAllocator {
    /// The region must be zeroed initially, aligned for HeapState, and exclusively owned by
    /// this allocator for the invocation. Heap operations never allocate recursively.
    unsafe fn heap(&self) -> *mut Heap {
        let state = self.start as *mut HeapState;
        unsafe {
            if (*state).initialized == 0 {
                let bottom = (self.start as *mut u8).add(size_of::<HeapState>());
                (*state)
                    .heap
                    .write(Heap::new(bottom, self.len - size_of::<HeapState>()));
                (*state).initialized = 1;
            }
            (*state).heap.as_mut_ptr()
        }
    }
}

// SAFETY: Solana runs each invocation on one thread with its own zeroed heap. The
// allocator's metadata and allocation region are disjoint, and neither escapes to a CPI.
unsafe impl GlobalAlloc for ReclaimingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe {
            (*self.heap())
                .allocate_first_fit(layout)
                .map_or(ptr::null_mut(), NonNull::as_ptr)
        }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { (*self.heap()).deallocate(NonNull::new_unchecked(pointer), layout) };
    }
}

#[cfg(all(
    target_os = "solana",
    feature = "custom-heap",
    not(feature = "no-entrypoint")
))]
#[global_allocator]
static ALLOCATOR: ReclaimingAllocator = ReclaimingAllocator {
    start: anchor_lang::solana_program::entrypoint::HEAP_START_ADDRESS as usize,
    len: anchor_lang::solana_program::entrypoint::HEAP_LENGTH,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn allocator() -> ReclaimingAllocator {
        // Leak an aligned region to satisfy Heap's lifetime contract. Each test owns its arena.
        let arena = Box::leak(Box::new([0usize; 4096]));
        ReclaimingAllocator {
            start: arena.as_mut_ptr() as usize,
            len: size_of::<[usize; 4096]>(),
        }
    }

    #[test]
    fn repeated_phases_reclaim_memory_without_touching_live_allocations() {
        let allocator = allocator();
        let retained = Layout::from_size_align(1024, 64).unwrap();
        let phase = Layout::from_size_align(12 * 1024, 256).unwrap();
        unsafe {
            let live = allocator.alloc(retained);
            assert!(!live.is_null());
            live.write_bytes(0xa5, retained.size());
            for _ in 0..20 {
                let temporary = allocator.alloc(phase);
                assert!(!temporary.is_null());
                assert_eq!(temporary as usize % phase.align(), 0);
                temporary.write_bytes(0xff, phase.size());
                allocator.dealloc(temporary, phase);
                assert!(
                    core::slice::from_raw_parts(live, retained.size())
                        .iter()
                        .all(|&value| value == 0xa5)
                );
            }
            allocator.dealloc(live, retained);
            assert_eq!((*allocator.heap()).used(), 0);
        }
    }

    #[test]
    fn exhaustion_recovers_after_adjacent_blocks_are_freed() {
        let allocator = allocator();
        let layout = Layout::from_size_align(8192, 8).unwrap();
        unsafe {
            let first = allocator.alloc(layout);
            let second = allocator.alloc(layout);
            let third = allocator.alloc(layout);
            assert!(!first.is_null() && !second.is_null() && !third.is_null());
            assert!(allocator.alloc(layout).is_null());
            allocator.dealloc(second, layout);
            allocator.dealloc(first, layout);
            allocator.dealloc(third, layout);
            let merged = Layout::from_size_align(30 * 1024, 8).unwrap();
            let large = allocator.alloc(merged);
            assert!(!large.is_null());
            assert!(large as usize >= allocator.start + size_of::<HeapState>());
            assert!(large as usize + merged.size() <= allocator.start + allocator.len);
            allocator.dealloc(large, merged);
        }
    }
}
