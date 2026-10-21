//! Per-operation cost of swansong's hot paths, with allocation counts.
//!
//! `scaling.rs` answers whether an operation scales; this answers what one operation costs and
//! whether it allocates (`docs/invariants.md`, rule H4). Each benchmark also runs on several
//! threads at once against one shared `Swansong`, so contention shows up as a rising per-op time.
//!
//! ```sh
//! cargo bench --bench ops
//! ```

use divan::{black_box, AllocProfiler, Bencher};
use std::{
    future::{ready, Future},
    pin::{pin, Pin},
    task::{Context, Poll, Waker},
};
use swansong::Swansong;

#[global_allocator]
static ALLOC: AllocProfiler = AllocProfiler::system();

fn main() {
    divan::main();
}

/// Pending on its first poll, ready on its second: the shape of a keep-alive read that waits
/// for the next request.
struct PendingOnce(bool);

impl Future for PendingOnce {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            Poll::Pending
        }
    }
}

/// `interrupt(fut)`, polled Pending then Ready, then dropped.
#[divan::bench(threads = [1, 2, 4, 8])]
fn interrupt_pending(bencher: Bencher) {
    let swansong = Swansong::new();
    bencher.bench(|| {
        let mut cx = Context::from_waker(Waker::noop());
        let mut interrupt = pin!(swansong.interrupt(PendingOnce(false)));
        let _ = black_box(interrupt.as_mut().poll(&mut cx));
        let _ = black_box(interrupt.as_mut().poll(&mut cx));
    });
}

/// `interrupt(fut)` around an already-ready future.
#[divan::bench(threads = [1, 2, 4, 8])]
fn interrupt_ready(bencher: Bencher) {
    let swansong = Swansong::new();
    bencher.bench(|| {
        let mut cx = Context::from_waker(Waker::noop());
        let mut interrupt = pin!(swansong.interrupt(ready(())));
        let _ = black_box(interrupt.as_mut().poll(&mut cx));
    });
}

/// `guard()` created and dropped.
#[divan::bench(threads = [1, 2, 4, 8])]
fn guard(bencher: Bencher) {
    let swansong = Swansong::new();
    bencher.bench(|| black_box(swansong.guard()));
}

/// `state()`.
#[divan::bench(threads = [1, 2, 4, 8])]
fn state(bencher: Bencher) {
    let swansong = Swansong::new();
    bencher.bench(|| black_box(swansong.state()));
}

/// `child()` with a guard on it, then both dropped.
#[divan::bench(threads = [1, 2, 4, 8])]
fn child_with_guard(bencher: Bencher) {
    let swansong = Swansong::new();
    bencher.bench(|| {
        let child = swansong.child();
        black_box(child.guard());
    });
}
