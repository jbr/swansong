//! Loom models of the stop and guard protocols (`docs/invariants.md` §3 and §7), driving the
//! real types. Run with `RUSTFLAGS="--cfg swansong_loom" cargo test --lib --release`.

use super::{shard::STOPPED, Guard, Inner};
use crate::Interrupt;
use loom::{
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};
use std::{
    future::{pending, Future, Pending},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

#[derive(Default)]
struct Wakes(AtomicUsize);

impl Wake for Wakes {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl Wakes {
    fn waker() -> (Arc<Self>, Waker) {
        let wake_count = Arc::new(Self::default());
        (Arc::clone(&wake_count), Waker::from(wake_count))
    }

    fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

fn poll(interrupt: &mut Interrupt<Pending<()>>, waker: &Waker) -> Poll<Option<()>> {
    Pin::new(interrupt).poll(&mut Context::from_waker(waker))
}

/// L1, L2: a poll racing stop either observes it or registers in time to be woken by it.
#[test]
fn register_vs_stop() {
    loom::model(|| {
        let root = Inner::new_root(false);
        let mut interrupt = Interrupt::new(&root, pending());
        let (wake_count, waker) = Wakes::waker();

        let stopper = thread::spawn({
            let root = Arc::clone(&root);
            move || root.stop()
        });
        let polled = poll(&mut interrupt, &waker);
        stopper.join().unwrap();

        if polled.is_pending() {
            assert!(
                wake_count.count() > 0,
                "pending interrupt not woken by stop"
            );
        }
        assert_eq!(poll(&mut interrupt, &waker), Poll::Ready(None));
    });
}

/// A re-poll from the same task skips the lock; the registration it relies on must still
/// be woken by a racing stop.
#[test]
fn same_waker_repoll_vs_stop() {
    loom::model(|| {
        let root = Inner::new_root(false);
        let mut interrupt = Interrupt::new(&root, pending());
        let (wake_count, waker) = Wakes::waker();
        assert!(poll(&mut interrupt, &waker).is_pending());

        let stopper = thread::spawn({
            let root = Arc::clone(&root);
            move || root.stop()
        });
        let polled = poll(&mut interrupt, &waker);
        stopper.join().unwrap();

        if polled.is_pending() {
            assert!(
                wake_count.count() > 0,
                "pending interrupt not woken by stop"
            );
        }
        assert_eq!(poll(&mut interrupt, &waker), Poll::Ready(None));
    });
}

/// L4: a registered interrupt migrates to another thread and is polled with a new waker
/// while stop drains its shard. If that poll returns Pending, the new waker must be woken.
#[test]
fn migration_and_waker_replacement_vs_stop() {
    loom::model(|| {
        let root = Inner::new_root(false);
        let mut interrupt = Interrupt::new(&root, pending());
        let (_, first_waker) = Wakes::waker();
        assert!(poll(&mut interrupt, &first_waker).is_pending());

        let stopper = thread::spawn({
            let root = Arc::clone(&root);
            move || root.stop()
        });
        let migrated = thread::spawn(move || {
            let (wake_count, waker) = Wakes::waker();
            let polled = poll(&mut interrupt, &waker);
            (interrupt, wake_count, waker, polled)
        });
        stopper.join().unwrap();
        let (mut interrupt, wake_count, waker, polled) = migrated.join().unwrap();

        if polled.is_pending() {
            assert!(
                wake_count.count() > 0,
                "replacement waker not woken by stop"
            );
        }
        assert_eq!(poll(&mut interrupt, &waker), Poll::Ready(None));
    });
}

/// Dropping one registration while stop drains must not disturb another's: the survivor is
/// woken, and no registration outlives the drain.
#[test]
fn deregistration_vs_stop() {
    loom::model(|| {
        let root = Inner::new_root(false);
        let mut dropped = Interrupt::new(&root, pending());
        let mut survivor = Interrupt::new(&root, pending());
        let (_, dropped_waker) = Wakes::waker();
        let (wake_count, survivor_waker) = Wakes::waker();
        assert!(poll(&mut dropped, &dropped_waker).is_pending());
        assert!(poll(&mut survivor, &survivor_waker).is_pending());

        let deregistering = thread::spawn(move || drop(dropped));
        root.stop();
        deregistering.join().unwrap();

        assert!(
            wake_count.count() > 0,
            "surviving interrupt not woken by stop"
        );
        for shard in root.shards() {
            assert_eq!(shard.waiter_count(), 0);
        }
        assert_eq!(poll(&mut survivor, &survivor_waker), Poll::Ready(None));
    });
}

/// `spec.md` §5.8: when a child is collected, an interrupt pending on it goes terminal and
/// is woken to observe that.
#[test]
fn collection_vs_pending_interrupt() {
    loom::model(|| {
        let root = Inner::new_root(false);
        let child = Inner::new_child(&root);
        let mut interrupt = Interrupt::new(&child, pending());
        let (wake_count, waker) = Wakes::waker();

        let collector = thread::spawn(move || drop(child));
        let polled = poll(&mut interrupt, &waker);
        collector.join().unwrap();

        if polled.is_pending() {
            assert!(
                wake_count.count() > 0,
                "interrupt on collected child not woken"
            );
        }
        assert_eq!(poll(&mut interrupt, &waker), Poll::Ready(None));
        assert!(root.shards().iter().all(|shard| !shard.is_stopped()));
    });
}

/// S3, which 0.3.6 violated: the second guard on an empty child relies on the first guard's
/// arrival at the root, and its creator then stops the root. That guard is alive and was
/// created before the stop, so the root must not look complete.
#[test]
fn piggybacked_guard_vs_stop() {
    loom::model(|| {
        let root = Inner::new_root(false);
        let child = Inner::new_child(&root);
        let first = thread::spawn({
            let child = Arc::clone(&child);
            move || Guard::new(&child)
        });
        let second = Guard::new(&child);
        root.stop();
        assert!(
            !root.is_complete(),
            "root complete while a guard created before the stop is alive"
        );

        drop(first.join().unwrap());
        drop(second);
        assert!(child.is_complete());
        assert!(root.is_complete());
    });
}

/// L3: the last guard drops while stop flushes. Afterwards the root is complete, and if the
/// completion check that follows the stop saw it incomplete, a notification came after it.
#[test]
fn last_guard_drop_vs_stop() {
    loom::model(|| {
        let root = Inner::new_root(false);
        let guard = Guard::new(&root);
        let dropper = thread::spawn(move || drop(guard));

        root.stop();
        let listened = root.zero_notifications();
        let complete = root.is_complete();
        dropper.join().unwrap();

        assert!(root.is_complete());
        if !complete {
            assert!(
                root.zero_notifications() > listened,
                "completion not notified"
            );
        }
    });
}

/// L3 across levels: a child's last guard, which pins the child, drops while the root stops.
#[test]
fn child_guard_drop_vs_root_stop() {
    loom::model(|| {
        let root = Inner::new_root(false);
        let guard = Guard::new(&Inner::new_child(&root));
        let dropper = thread::spawn(move || drop(guard));

        root.stop();
        let listened = root.zero_notifications();
        let complete = root.is_complete();
        dropper.join().unwrap();

        assert!(root.is_complete());
        if !complete {
            assert!(
                root.zero_notifications() > listened,
                "completion not notified"
            );
        }
    });
}

/// Two first guards race to make an empty child arrive. One arrival wins and the other is
/// taken back, so the counts come out exact.
#[test]
fn concurrent_first_arrivals() {
    loom::model(|| {
        let root = Inner::new_root(false);
        let child = Inner::new_child(&root);
        let other = thread::spawn({
            let child = Arc::clone(&child);
            move || Guard::new(&child)
        });
        let guard = Guard::new(&child);
        let other = other.join().unwrap();
        assert!(!child.is_zero() && !root.is_zero());
        assert_eq!(root.guard_count_subtree(), 2);

        drop((guard, other));
        assert_eq!(root.guard_count_subtree(), 0);
        root.stop();
        assert!(child.is_complete() && root.is_complete());
        for shard in root.shards() {
            assert_eq!(shard.state(), STOPPED, "flushed and empty");
        }
    });
}

/// A guard arrives at a running child's empty, still-arrived shard while the child stops and
/// flushes it. Whichever wins, the guard is counted at every level.
#[test]
fn arrival_vs_flush() {
    loom::model(|| {
        let root = Inner::new_root(false);
        let child = Inner::new_child(&root);
        drop(Guard::new(&child));
        let creator = thread::spawn({
            let child = Arc::clone(&child);
            move || Guard::new(&child)
        });

        child.stop();
        let guard = creator.join().unwrap();
        assert!(!child.is_zero() && !root.is_zero());

        drop(guard);
        assert!(child.is_complete());
        root.stop();
        assert!(root.is_complete());
    });
}

/// A child whose guards have all dropped is collected while the root stops. Either way it stops
/// counting toward the root, which completes (`spec.md` §5.8).
#[test]
fn collection_vs_root_stop() {
    loom::model(|| {
        let root = Inner::new_root(false);
        let child = Inner::new_child(&root);
        drop(Guard::new(&child));
        let collector = thread::spawn(move || drop(child));

        root.stop();
        let listened = root.zero_notifications();
        let complete = root.is_complete();
        collector.join().unwrap();

        assert!(root.is_complete());
        if !complete {
            assert!(
                root.zero_notifications() > listened,
                "completion not notified"
            );
        }
    });
}

/// `spec.md` §5.6: a child created while its parent stops ends up stopped, whichever is first.
#[test]
fn child_creation_vs_parent_stop() {
    loom::model(|| {
        let root = Inner::new_root(false);
        let creator = thread::spawn({
            let root = Arc::clone(&root);
            move || Inner::new_child(&root)
        });
        root.stop();
        let child = creator.join().unwrap();
        assert!(child.is_stopped());
    });
}

/// A child is collected while a sibling is created, whose insertion may sweep the collected
/// child's entry. Stop still reaches the sibling, and the collected child departs the root.
#[test]
fn sibling_creation_vs_collection() {
    loom::model(|| {
        let root = Inner::new_root(false);
        let doomed = Inner::new_child(&root);
        drop(Guard::new(&doomed));
        let collector = thread::spawn(move || drop(doomed));
        let sibling = Inner::new_child(&root);
        collector.join().unwrap();

        root.stop();
        assert!(sibling.is_stopped());
        assert!(root.is_complete());
    });
}
