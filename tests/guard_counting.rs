//! Guard accounting across threads and tree levels: guards are counted on per-thread shards,
//! and a count becomes visible only once every level above it counts it (`docs/invariants.md`
//! S3, L3).

use std::{
    future::{Future, IntoFuture},
    pin::pin,
    sync::{mpsc, Arc, Barrier},
    task::{Context, Waker},
    thread,
};
use swansong::{Guard, Swansong};

fn is_complete(swansong: &Swansong) -> bool {
    let mut completion = pin!(swansong.clone().into_future());
    completion
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_ready()
}

#[test]
fn piggybacked_guard_delays_root_completion() {
    // Two threads create the first guards on an empty child at once. The second sees the child
    // already nonempty; when its guard() returns, the root must count it, so the root's
    // completion can't resolve while it lives. 0.3.6 resolved it about once in 100k rounds.
    #[cfg(miri)]
    const ROUNDS: usize = 20;
    #[cfg(not(miri))]
    const ROUNDS: usize = 10_000;

    let barrier = Arc::new(Barrier::new(2));
    let (to_other, from_main) = mpsc::channel::<Option<Swansong>>();
    let (to_main, from_other) = mpsc::channel::<Guard>();
    let other = thread::spawn({
        let barrier = Arc::clone(&barrier);
        move || {
            while let Ok(Some(child)) = from_main.recv() {
                barrier.wait();
                to_main.send(child.guard()).unwrap();
            }
        }
    });

    for _ in 0..ROUNDS {
        let root = Swansong::new();
        let child = root.child();
        to_other.send(Some(child.clone())).unwrap();
        barrier.wait();
        let guard = child.guard();
        root.shut_down();
        assert!(
            !is_complete(&root),
            "root complete while a guard created before shut_down was alive"
        );
        drop(guard);
        drop(from_other.recv().unwrap());
        assert!(is_complete(&root));
    }
    to_other.send(None).unwrap();
    other.join().unwrap();
}

#[test]
fn guards_move_between_threads() {
    let root = Swansong::new();
    let child = root.child();
    let guards: Vec<Guard> = (0..8)
        .map(|i| {
            let (root, child) = (root.clone(), child.clone());
            thread::spawn(move || {
                if i % 2 == 0 {
                    root.guard()
                } else {
                    child.guard()
                }
            })
            .join()
            .unwrap()
        })
        .collect();
    assert_eq!(root.guard_count(), 8);
    assert_eq!(child.guard_count(), 4);

    // Drop each guard on a thread other than the one that created it, and clone some first.
    let clones: Vec<Guard> = guards.iter().step_by(3).cloned().collect();
    assert_eq!(root.guard_count(), 8 + clones.len());
    thread::spawn(move || drop(guards)).join().unwrap();
    assert_eq!(root.guard_count(), clones.len());

    root.shut_down();
    assert!(!is_complete(&root));
    thread::spawn(move || drop(clones)).join().unwrap();
    assert_eq!(root.guard_count(), 0);
    assert!(is_complete(&root));
    assert!(is_complete(&child));
}

#[test]
fn guard_created_after_shut_down_is_counted() {
    let root = Swansong::new();
    let child = root.child();
    drop(child.guard());
    root.shut_down();
    assert!(is_complete(&root));

    let guard = thread::spawn({
        let child = child.clone();
        move || child.guard()
    })
    .join()
    .unwrap();
    assert!(root.state().is_shutting_down());
    assert!(!is_complete(&child));
    drop(guard);
    assert!(root.state().is_complete());
    assert!(child.state().is_complete());
}

#[test]
fn stress_guards_racing_shutdown() {
    // Threads keep creating and dropping guards on a shared root and on per-thread children,
    // holding some, while another thread shuts the root down. Once every guard is dropped the
    // root must complete (L3), with nothing left counted.
    #[cfg(miri)]
    let (rounds, threads) = (2usize, 2usize);
    #[cfg(not(miri))]
    let (rounds, threads) = (100usize, 4usize);

    for _ in 0..rounds {
        let root = Swansong::new();
        let start = Arc::new(Barrier::new(threads + 1));
        let workers: Vec<_> = (0..threads)
            .map(|worker| {
                let (root, start) = (root.clone(), Arc::clone(&start));
                thread::spawn(move || {
                    let child = root.child();
                    let mut held = vec![];
                    start.wait();
                    for i in 0..64 {
                        let guard = if (i + worker) % 2 == 0 {
                            root.guard()
                        } else {
                            child.guard()
                        };
                        if i % 5 == 0 {
                            held.push(guard);
                        }
                    }
                    held
                })
            })
            .collect();

        start.wait();
        let completion = root.shut_down();
        let held: Vec<Guard> = workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(root.guard_count(), held.len());
        drop(held);
        assert_eq!(root.guard_count(), 0);

        let (done, finished) = mpsc::channel();
        thread::spawn(move || {
            completion.block();
            done.send(()).unwrap();
        });
        #[cfg(miri)]
        finished.recv().unwrap();
        #[cfg(not(miri))]
        finished
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("shutdown never completed");
    }
}

#[test]
fn shared_root_guards_count_exactly() {
    #[cfg(miri)]
    const THREADS: usize = 3;
    #[cfg(not(miri))]
    const THREADS: usize = 16;

    let root = Swansong::new();
    let created = Arc::new(Barrier::new(THREADS + 1));
    let counted = Arc::new(Barrier::new(THREADS + 1));
    let threads: Vec<_> = (0..THREADS)
        .map(|_| {
            let (root, created, counted) =
                (root.clone(), Arc::clone(&created), Arc::clone(&counted));
            thread::spawn(move || {
                let guards: Vec<_> = (0..10).map(|_| root.guard()).collect();
                created.wait();
                counted.wait();
                drop(guards);
            })
        })
        .collect();

    created.wait();
    assert_eq!(root.guard_count(), THREADS * 10);
    assert_eq!(root.guard_report().guard_count(), THREADS * 10);
    counted.wait();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(root.guard_count(), 0);
    assert!(root.guard_report().is_empty());
}
