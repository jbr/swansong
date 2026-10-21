//! Without a guard report, creating and dropping a guard allocates nothing once the shard is
//! warm: provenance is opt-in (`Swansong::with_guard_report`).

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};
use swansong::Swansong;

struct Counting;

thread_local! {
    // Per thread, since the test harness runs tests concurrently.
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocations_during(f: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.with(Cell::get);
    f();
    ALLOCATIONS.with(Cell::get) - before
}

#[test]
fn guards_without_a_report_do_not_allocate() {
    let root = Swansong::new();
    let child = root.child();
    // Warm up: first use of a shard may allocate thread-local and slab state.
    drop((root.guard(), child.guard()));

    let allocations = allocations_during(|| {
        for _ in 0..100 {
            let guards = (root.guard(), child.guard());
            let clones = (guards.0.clone(), guards.1.clone());
            drop((guards, clones));
        }
    });
    assert_eq!(allocations, 0);
}

#[test]
fn guards_with_a_report_record_provenance() {
    let root = Swansong::with_guard_report();
    drop(root.guard());
    assert!(allocations_during(|| drop(root.guard())) > 0);
}
