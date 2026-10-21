//! How `Interrupt<Future>` and `Interrupt<Stream>` register for stop wakeups: lazily, once per
//! task, across threads, and with every registration woken at stop. See `docs/invariants.md` §3.
//!
//! Wakers here count their wakes, and `Arc::strong_count` on the counter shows how many clones of
//! the waker swansong is holding.

use futures_lite::Stream;
use std::{
    future::{pending, ready, Future},
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Barrier,
    },
    task::{Context, Poll, Wake, Waker},
    thread,
};
use swansong::{Interrupt, Swansong};

#[derive(Default)]
struct Wakes(AtomicUsize);

impl Wake for Wakes {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn counting_waker() -> (Arc<Wakes>, Waker) {
    let wakes = Arc::new(Wakes::default());
    (Arc::clone(&wakes), Waker::from(wakes))
}

impl Wakes {
    fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

/// Clones of `waker` held by anyone other than the caller's `Arc<Wakes>` and `Waker`.
fn held_clones(wakes: &Arc<Wakes>) -> usize {
    Arc::strong_count(wakes) - 2
}

fn poll<F: Future + Unpin>(future: &mut F, waker: &Waker) -> Poll<F::Output> {
    Pin::new(future).poll(&mut Context::from_waker(waker))
}

fn poll_next<S: Stream + Unpin>(stream: &mut S, waker: &Waker) -> Poll<Option<S::Item>> {
    Pin::new(stream).poll_next(&mut Context::from_waker(waker))
}

#[test]
fn ready_future_does_not_register() {
    let swansong = Swansong::new();
    let (wakes, waker) = counting_waker();
    let mut interrupt = swansong.interrupt(ready(1));
    assert_eq!(poll(&mut interrupt, &waker), Poll::Ready(Some(1)));
    assert_eq!(held_clones(&wakes), 0);
}

#[test]
fn pending_future_registers_once_per_task() {
    let swansong = Swansong::new();
    let (wakes, waker) = counting_waker();
    let mut interrupt = swansong.interrupt(pending::<()>());
    assert!(poll(&mut interrupt, &waker).is_pending());
    let registered = held_clones(&wakes);
    assert!(registered > 0);

    for _ in 0..3 {
        assert!(poll(&mut interrupt, &waker.clone()).is_pending());
    }
    assert_eq!(held_clones(&wakes), registered);

    drop(interrupt);
    assert_eq!(held_clones(&wakes), 0);
    assert_eq!(wakes.count(), 0);
}

#[test]
fn stop_wakes_the_latest_waker() {
    let swansong = Swansong::new();
    let (first_wakes, first_waker) = counting_waker();
    let (second_wakes, second_waker) = counting_waker();
    let mut interrupt = swansong.interrupt(pending::<()>());

    assert!(poll(&mut interrupt, &first_waker).is_pending());
    assert!(poll(&mut interrupt, &second_waker).is_pending());
    assert_eq!(held_clones(&first_wakes), 0, "replaced waker released");

    swansong.shut_down();
    assert_eq!(first_wakes.count(), 0);
    assert_eq!(second_wakes.count(), 1);
    assert_eq!(poll(&mut interrupt, &second_waker), Poll::Ready(None));
}

#[test]
fn stop_wakes_pending_streams() {
    let swansong = Swansong::new();
    let (wakes, waker) = counting_waker();
    let mut stream = swansong.interrupt(futures_lite::stream::pending::<()>());
    assert!(poll_next(&mut stream, &waker).is_pending());
    swansong.shut_down();
    assert_eq!(wakes.count(), 1);
    assert_eq!(poll_next(&mut stream, &waker), Poll::Ready(None));
}

#[test]
fn shut_down_does_not_wake_dropped_interrupts() {
    let swansong = Swansong::new();
    let (wakes, waker) = counting_waker();
    let mut interrupt = swansong.interrupt(pending::<()>());
    assert!(poll(&mut interrupt, &waker).is_pending());
    drop(interrupt);
    swansong.shut_down();
    assert_eq!(wakes.count(), 0);
}

#[test]
fn polling_after_shut_down_does_not_register() {
    let swansong = Swansong::new();
    swansong.shut_down();
    let (wakes, waker) = counting_waker();
    let mut interrupt = swansong.interrupt(pending::<()>());
    assert_eq!(poll(&mut interrupt, &waker), Poll::Ready(None));
    assert_eq!(held_clones(&wakes), 0);
}

#[test]
fn collected_child_wakes_pending_interrupts() {
    // spec.md §5.8: once nothing pins the child, it is collected and its interrupts go terminal.
    let root = Swansong::new();
    let child = root.child();
    let (wakes, waker) = counting_waker();
    let mut interrupt = child.interrupt(pending::<()>());
    assert!(poll(&mut interrupt, &waker).is_pending());

    drop(child);
    assert_eq!(wakes.count(), 1);
    assert_eq!(poll(&mut interrupt, &waker), Poll::Ready(None));
    assert!(root.state().is_running());
}

#[test]
fn guarded_child_keeps_interrupts_running() {
    let root = Swansong::new();
    let child = root.child();
    let guard = child.guard();
    let (wakes, waker) = counting_waker();
    let mut interrupt = child.interrupt(pending::<()>());
    assert!(poll(&mut interrupt, &waker).is_pending());

    drop(child);
    assert_eq!(wakes.count(), 0);
    assert!(poll(&mut interrupt, &waker).is_pending());

    drop(guard);
    assert_eq!(wakes.count(), 1);
    assert_eq!(poll(&mut interrupt, &waker), Poll::Ready(None));
}

#[test]
fn interrupts_from_different_threads_compare_equal() {
    let swansong = Swansong::new();
    let other = Swansong::new();
    let here = swansong.interrupt(1);
    let (there, elsewhere) = thread::spawn({
        let swansong = swansong.clone();
        move || (swansong.interrupt(1), other.interrupt(1))
    })
    .join()
    .unwrap();
    assert_eq!(here, there);
    assert_ne!(here, elsewhere);
}

#[test]
fn guarded_interrupt_from_another_thread_guards_its_swansong() {
    let swansong = Swansong::new();
    let interrupt = thread::spawn({
        let swansong = swansong.clone();
        move || swansong.interrupt(pending::<()>()).guarded()
    })
    .join()
    .unwrap();
    assert_eq!(swansong.guard_count(), 1);
    drop(interrupt);
    assert_eq!(swansong.guard_count(), 0);
}

#[test]
fn migrated_interrupt_is_woken() {
    // Registered on one thread, then moved to and polled from another with a new waker.
    let swansong = Swansong::new();
    let (_, first_waker) = counting_waker();
    let mut interrupt = swansong.interrupt(pending::<()>());
    assert!(poll(&mut interrupt, &first_waker).is_pending());

    let (wakes, waker, mut interrupt) = thread::spawn(move || {
        let (wakes, waker) = counting_waker();
        assert!(poll(&mut interrupt, &waker).is_pending());
        (wakes, waker, interrupt)
    })
    .join()
    .unwrap();

    swansong.shut_down();
    assert_eq!(wakes.count(), 1);
    assert_eq!(poll(&mut interrupt, &waker), Poll::Ready(None));
}

#[test]
fn shut_down_wakes_interrupts_on_every_thread() {
    #[cfg(miri)]
    const THREADS: usize = 3;
    #[cfg(not(miri))]
    const THREADS: usize = 16;

    let swansong = Swansong::new();
    let registered = Arc::new(Barrier::new(THREADS + 1));
    let stopped = Arc::new(Barrier::new(THREADS + 1));
    let threads: Vec<_> = (0..THREADS)
        .map(|_| {
            let (swansong, registered, stopped) =
                (swansong.clone(), registered.clone(), stopped.clone());
            thread::spawn(move || {
                let (wakes, waker) = counting_waker();
                let mut interrupt = swansong.interrupt(pending::<()>());
                assert!(poll(&mut interrupt, &waker).is_pending());
                registered.wait();
                stopped.wait();
                assert_eq!(wakes.count(), 1);
                assert_eq!(poll(&mut interrupt, &waker), Poll::Ready(None));
            })
        })
        .collect();

    registered.wait();
    swansong.shut_down();
    stopped.wait();
    for thread in threads {
        thread.join().unwrap();
    }
}

#[test]
fn stress_registration_racing_shutdown() {
    // Threads keep creating, polling, re-polling and dropping interrupts while another thread
    // shuts down. Every interrupt still pending afterwards must have been woken (L1), and must
    // then observe the stop.
    #[cfg(miri)]
    let (rounds, threads) = (2usize, 2usize);
    #[cfg(not(miri))]
    let (rounds, threads) = (200usize, 4usize);

    for _ in 0..rounds {
        let swansong = Swansong::new();
        let start = Arc::new(Barrier::new(threads + 1));
        let stop_seen = Arc::new(AtomicBool::new(false));
        let workers: Vec<_> = (0..threads)
            .map(|worker| {
                let (swansong, start, stop_seen) =
                    (swansong.clone(), start.clone(), stop_seen.clone());
                thread::spawn(move || {
                    let mut held: Vec<(Interrupt<_>, Arc<Wakes>, Waker)> = vec![];
                    start.wait();
                    for i in 0.. {
                        let (wakes, waker) = counting_waker();
                        let mut interrupt = swansong.interrupt(pending::<()>());
                        match poll(&mut interrupt, &waker) {
                            Poll::Ready(None) => break,
                            Poll::Ready(Some(())) => unreachable!(),
                            Poll::Pending => {}
                        }
                        if (i + worker) % 3 == 0 {
                            // re-poll from a different task
                            let (wakes, waker) = counting_waker();
                            if poll(&mut interrupt, &waker).is_pending() {
                                held.push((interrupt, wakes, waker));
                            }
                        } else if (i + worker) % 3 == 1 {
                            held.push((interrupt, wakes, waker));
                        }
                        if stop_seen.load(Ordering::Relaxed) && i > 8 {
                            break;
                        }
                    }
                    held
                })
            })
            .collect();

        start.wait();
        thread::yield_now();
        swansong.shut_down();
        stop_seen.store(true, Ordering::Relaxed);

        for worker in workers {
            for (mut interrupt, wakes, waker) in worker.join().unwrap() {
                assert_eq!(wakes.count(), 1, "pending interrupt not woken by shut_down");
                assert_eq!(poll(&mut interrupt, &waker), Poll::Ready(None));
            }
        }
    }
}
