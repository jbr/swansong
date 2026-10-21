//! Thread-scaling harness for swansong's hot paths.
//!
//! For each operation, reports aggregate throughput (millions of operations per second, summed
//! across threads) as the thread count rises, with every thread sharing one `Swansong` versus
//! each thread owning its own. A shared column that tracks the private one scales; one that
//! falls away from it is a serialization point. See `docs/invariants.md`, section 5.
//!
//! ```sh
//! cargo bench --bench scaling -- [--threads 1,2,4,8] [--ops guard,interrupt-pending] [--millis 500]
//! ```
//!
//! Pin with `taskset` to compare core layouts, e.g. every thread inside one L3 versus the same
//! number of threads spread across several.

use std::{
    future::{ready, Future},
    hint::black_box,
    pin::{pin, Pin},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Barrier,
    },
    task::{Context, Poll, Waker},
    thread,
    time::{Duration, Instant},
};
use swansong::Swansong;

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

#[derive(Clone, Copy)]
enum Op {
    /// `interrupt(fut)`, polled Pending then Ready, then dropped. The h1 keep-alive head read.
    InterruptPending,
    /// `interrupt(fut)` around an already-ready future. Should never need to register.
    InterruptReady,
    /// `guard()` created and dropped. The per-stream h2/h3 pattern.
    Guard,
    /// `state()`, read on every h1 response.
    State,
    /// `child()` with a guard and a pending interrupt on it, then all dropped. The
    /// per-connection h2/h3 pattern.
    Child,
}

const OPS: [(&str, Op); 5] = [
    ("interrupt-pending", Op::InterruptPending),
    ("interrupt-ready", Op::InterruptReady),
    ("guard", Op::Guard),
    ("state", Op::State),
    ("child", Op::Child),
];

fn run_once(op: Op, swansong: &Swansong, cx: &mut Context<'_>) {
    match op {
        Op::InterruptPending => {
            let mut interrupt = pin!(swansong.interrupt(PendingOnce(false)));
            let _ = black_box(interrupt.as_mut().poll(cx));
            let _ = black_box(interrupt.as_mut().poll(cx));
        }
        Op::InterruptReady => {
            let mut interrupt = pin!(swansong.interrupt(ready(())));
            let _ = black_box(interrupt.as_mut().poll(cx));
        }
        Op::Guard => {
            black_box(swansong.guard());
        }
        Op::State => {
            black_box(swansong.state());
        }
        Op::Child => {
            let child = swansong.child();
            let guard = child.guard();
            let mut interrupt = pin!(child.interrupt(PendingOnce(false)));
            let _ = black_box(interrupt.as_mut().poll(cx));
            let _ = black_box(interrupt.as_mut().poll(cx));
            drop(guard);
        }
    }
}

/// Aggregate millions of operations per second for `threads` threads running `op` for
/// `duration`, all on one `Swansong` if `shared`, otherwise one per thread.
fn measure(op: Op, threads: usize, shared: bool, duration: Duration) -> f64 {
    let root = Swansong::new();
    let start_line = Arc::new(Barrier::new(threads + 1));
    let stop = Arc::new(AtomicBool::new(false));
    let total = Arc::new(AtomicU64::new(0));

    let handles: Vec<_> = (0..threads)
        .map(|_| {
            let swansong = if shared {
                root.clone()
            } else {
                Swansong::new()
            };
            let (start_line, stop, total) = (start_line.clone(), stop.clone(), total.clone());
            thread::spawn(move || {
                let mut cx = Context::from_waker(Waker::noop());
                for _ in 0..10_000 {
                    run_once(op, &swansong, &mut cx);
                }
                start_line.wait();
                let mut count = 0_u64;
                while !stop.load(Ordering::Relaxed) {
                    for _ in 0..256 {
                        run_once(op, &swansong, &mut cx);
                    }
                    count += 256;
                }
                total.fetch_add(count, Ordering::Relaxed);
            })
        })
        .collect();

    start_line.wait();
    let started = Instant::now();
    thread::sleep(duration);
    stop.store(true, Ordering::Relaxed);
    for handle in handles {
        handle.join().expect("bench thread panicked");
    }
    let elapsed = started.elapsed().as_secs_f64();

    #[allow(clippy::cast_precision_loss)]
    let ops = total.load(Ordering::Relaxed) as f64;
    ops / elapsed / 1e6
}

fn default_threads() -> Vec<usize> {
    let max = thread::available_parallelism().map_or(1, usize::from);
    let mut threads: Vec<usize> = std::iter::successors(Some(1_usize), |n| n.checked_mul(2))
        .take_while(|n| *n < max)
        .collect();
    threads.push(max);
    threads
}

fn main() {
    let mut threads = default_threads();
    let mut ops: Vec<(&str, Op)> = OPS.to_vec();
    let mut duration = Duration::from_millis(500);

    // cargo passes `--bench`; anything else unrecognized is ignored too.
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--threads" => {
                let list = args.next().expect("--threads takes a comma-separated list");
                threads = list
                    .split(',')
                    .map(|n| n.trim().parse().expect("thread counts are integers"))
                    .collect();
            }
            "--ops" => {
                let list = args.next().expect("--ops takes a comma-separated list");
                ops = list
                    .split(',')
                    .map(|name| {
                        *OPS.iter()
                            .find(|(known, _)| *known == name.trim())
                            .unwrap_or_else(|| panic!("unknown op {name}"))
                    })
                    .collect();
            }
            "--millis" => {
                let millis = args.next().expect("--millis takes an integer");
                duration = Duration::from_millis(millis.parse().expect("--millis is an integer"));
            }
            _ => {}
        }
    }

    println!(
        "available parallelism: {}; {} ms per measurement; Mops/s summed across threads",
        thread::available_parallelism().map_or(1, usize::from),
        duration.as_millis()
    );

    for (name, op) in ops {
        println!("\n{name}");
        println!(
            "{:>7} {:>10} {:>10} {:>15}",
            "threads", "shared", "private", "shared/private"
        );
        for &count in &threads {
            let shared = measure(op, count, true, duration);
            let private = measure(op, count, false, duration);
            println!(
                "{count:>7} {shared:>10.2} {private:>10.2} {:>15.2}",
                shared / private
            );
        }
    }
}
