use futures_lite::{Stream, StreamExt};
use std::{
    env,
    future::{self, Future, IntoFuture},
    ops::Add,
    pin::{pin, Pin},
    process::Termination,
    sync::{
        atomic::{AtomicU8, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll},
    thread::{self, sleep},
    time::Duration,
};
use swansong::Swansong;
use test_harness::test;

#[cfg(not(miri))]
use futures_lite::FutureExt;

#[cfg(not(miri))]
const TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(all(feature = "tokio", not(miri)))]
mod runtime {
    use std::{future::Future, time::Duration};
    pub(super) fn spawn(future: impl Future + Send + 'static) {
        tokio::task::spawn(async move {
            future.await;
        });
    }

    pub(super) fn block_on<T>(future: impl Future<Output = T>) -> T {
        tokio::runtime::Runtime::new().unwrap().block_on(future)
    }
    pub(super) async fn sleep(duration: Duration) {
        tokio::time::sleep(duration).await;
    }

    pub(super) fn interval(duration: Duration) -> tokio_stream::wrappers::IntervalStream {
        tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(duration))
    }
}

#[cfg(not(any(feature = "tokio", miri)))]
mod runtime {
    use std::{future::Future, time::Duration};

    pub(super) fn interval(duration: Duration) -> async_io::Timer {
        async_io::Timer::interval(duration)
    }

    pub(super) use async_global_executor::block_on;
    pub(super) fn spawn(future: impl Future + Send + 'static) {
        async_global_executor::spawn(async move {
            future.await;
        })
        .detach();
    }
    pub(super) async fn sleep(duration: Duration) {
        async_io::Timer::after(duration).await;
    }
}

#[cfg(miri)]
mod runtime {
    use futures_lite::stream::Stream;
    use std::{future::Future, thread, time::Duration};

    pub(super) fn spawn(fut: impl Future + Send + 'static) {
        thread::spawn(move || {
            block_on(async move {
                fut.await;
            })
        });
    }
    pub(super) async fn sleep(duration: Duration) {
        let (send, receive) = flume::unbounded();
        let jh = thread::spawn(move || {
            thread::sleep(duration);
            let _ = send.send(());
        });
        receive.recv_async().await.unwrap();
        jh.join().unwrap();
    }

    pub(super) fn interval(period: Duration) -> impl Stream<Item = ()> + Unpin + Send + 'static {
        let (send, receive) = flume::unbounded();
        thread::spawn(move || loop {
            thread::sleep(period);
            if send.send(()).is_err() {
                break;
            }
        });

        receive.into_stream()
    }

    pub(super) fn block_on<Fut: Future>(fut: Fut) -> Fut::Output {
        futures_lite::future::block_on(fut)
    }
}

#[track_caller]
fn harness<F, Fut, O>(test: F) -> O
where
    F: FnOnce() -> Fut,
    O: Termination,
    Fut: Future<Output = O> + Send,
{
    if let Some(seed) = env::var("TEST_SEED")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    {
        fastrand::seed(seed);
    } else {
        let seed = fastrand::get_seed();
        println!("TEST_SEED={seed}");
    }
    let _ = env_logger::builder().is_test(true).try_init();
    #[cfg(not(miri))]
    let res = runtime::block_on(async move { Some(test().await) }.race(async {
        runtime::sleep(TIMEOUT).await;
        None
    }))
    .expect("timed out");

    #[cfg(miri)]
    let res = runtime::block_on(test());

    res
}

async fn poll_manually<F: Future>(mut future: Pin<&mut F>) -> Poll<F::Output> {
    future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await
}

#[test(harness)]
async fn swansong() {
    let swansong = Swansong::new();
    let mut future = pin!(swansong.clone().into_future());

    assert!(poll_manually(future.as_mut()).await.is_pending());
    let guard = swansong.guard();
    let guard2 = guard.clone();
    assert_eq!(swansong.guard_count(), 2);
    assert!(swansong.state().is_running());
    assert!(poll_manually(future.as_mut()).await.is_pending());
    swansong.shut_down();
    assert!(swansong.state().is_shutting_down());
    assert!(poll_manually(future.as_mut()).await.is_pending());
    drop(guard);
    assert!(swansong.state().is_shutting_down());
    assert!(poll_manually(future.as_mut()).await.is_pending());
    drop(guard2);
    assert!(poll_manually(future.as_mut()).await.is_ready());
    assert!(swansong.state().is_complete());
}

#[test(harness)]
async fn multi_threaded() {
    let swansong = Swansong::new();
    let finished_count = Arc::new(AtomicU8::new(0));
    let expected_count = fastrand::u8(1..);
    let mut threads = vec![];

    for _ in 0..expected_count {
        let guard = swansong.guard();
        let finished_count = finished_count.clone();
        threads.push(thread::spawn(move || {
            let _guard = guard;
            sleep(Duration::from_millis(fastrand::u64(1..500)));
            finished_count.fetch_add(1, Ordering::Relaxed);
        }));
    }

    threads.push(thread::spawn({
        let swansong = swansong.clone();
        move || {
            sleep(Duration::from_millis(fastrand::u64(1..500)));
            swansong.shut_down();
        }
    }));

    swansong.await;

    assert_eq!(finished_count.load(Ordering::Relaxed), expected_count);
    for thread in threads {
        thread.join().unwrap();
    }
}

#[test]
fn multi_threaded_blocking() {
    let _ = env_logger::builder().is_test(true).try_init();
    let swansong = Swansong::new();
    let finished_count = Arc::new(AtomicU8::new(0));
    let expected_count = fastrand::u8(1..);
    let mut threads = vec![];

    for _ in 0..expected_count {
        let guard = swansong.guard();
        let finished_count = finished_count.clone();
        threads.push(thread::spawn(move || {
            let _guard = guard;
            sleep(Duration::from_millis(fastrand::u64(1..500)));
            finished_count.fetch_add(1, Ordering::Relaxed);
        }));
    }

    threads.push(thread::spawn({
        let swansong = swansong.clone();
        move || {
            sleep(Duration::from_millis(fastrand::u64(1..500)));
            swansong.shut_down();
        }
    }));

    let (send, receive) = std::sync::mpsc::channel();
    thread::spawn(move || {
        swansong.block_on_shutdown_completion();
        send.send(()).unwrap();
    });

    #[cfg(miri)]
    receive.recv().unwrap();

    #[cfg(not(miri))]
    receive.recv_timeout(Duration::from_secs(5)).unwrap();

    assert_eq!(finished_count.load(Ordering::Relaxed), expected_count);
    for thread in threads {
        thread.join().unwrap();
    }
}

#[test(harness)]
async fn future() {
    struct Fut(bool);
    impl Future for Fut {
        type Output = ();
        fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
            if self.0 {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }
    }
    impl Fut {
        fn ready(&mut self) {
            self.0 = true;
        }
    }

    let swansong = Swansong::new();
    let mut future = swansong.interrupt(Fut(false));
    assert!(poll_manually(Pin::new(&mut future)).await.is_pending());
    swansong.shut_down();
    assert_eq!(
        poll_manually(Pin::new(&mut future)).await,
        Poll::Ready(None)
    );

    let swansong = Swansong::new();
    let mut future = swansong.interrupt(Fut(false));
    assert!(poll_manually(Pin::new(&mut future)).await.is_pending());
    future.ready();
    assert_eq!(
        poll_manually(Pin::new(&mut future)).await,
        Poll::Ready(Some(()))
    );
}

#[test(harness)]
async fn stream() {
    struct Stream_(bool);
    impl Stream for Stream_ {
        type Item = ();
        fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            if self.0 {
                Poll::Ready(None)
            } else {
                Poll::Ready(Some(()))
            }
        }
    }
    impl Stream_ {
        fn ready(&mut self) {
            self.0 = true;
        }
    }

    let swansong = Swansong::new();
    let mut stream = swansong.interrupt(Stream_(false));
    assert_eq!(stream.next().await, Some(()));
    assert_eq!(stream.next().await, Some(()));
    swansong.shut_down();
    assert_eq!(stream.next().await, None);

    let swansong = Swansong::new();
    let mut stream = swansong.interrupt(Stream_(false));
    assert_eq!(stream.next().await, Some(()));
    assert_eq!(stream.next().await, Some(()));
    stream.ready();
    assert_eq!(stream.next().await, None);
    swansong.shut_down();
    assert_eq!(stream.next().await, None);
}

#[test(harness)]
async fn multi_threaded_future_guarded() {
    let swansong = Swansong::new();
    let canceled_count = Arc::new(AtomicU8::new(0));
    let finished_count = Arc::new(AtomicU8::new(0));
    let expected_count = fastrand::u8(1..);

    for _ in 0..expected_count {
        let finished_count = finished_count.clone();
        let canceled_count = canceled_count.clone();
        let fut = swansong.interrupt(async move {
            for _ in 0..fastrand::u8(1..5) {
                runtime::sleep(Duration::from_millis(fastrand::u64(1..100))).await;
            }
            finished_count.fetch_add(1, Ordering::Relaxed);
        });

        runtime::spawn(swansong.guarded(async move {
            let res = fut.await;
            runtime::sleep(Duration::from_millis(fastrand::u64(1..250))).await;
            if res.is_none() {
                canceled_count.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    runtime::spawn({
        let swansong = swansong.clone();
        async move {
            runtime::sleep(Duration::from_millis(fastrand::u64(1..100))).await;
            swansong.shut_down();
        }
    });

    swansong.await;

    assert_eq!(
        expected_count,
        finished_count.load(Ordering::Relaxed) + canceled_count.load(Ordering::Relaxed)
    );
}

#[test(harness)]
async fn multi_threaded_stream_guarded() {
    let swansong = Swansong::new();
    let finished_count = Arc::new(AtomicU8::new(0));
    let expected_count = fastrand::u8(1..);
    for _ in 0..expected_count {
        let finished_count = finished_count.clone();
        let mut stream = swansong.interrupt(runtime::interval(Duration::from_millis(
            fastrand::u64(1..100),
        )));

        runtime::spawn(swansong.guarded(async move {
            while (stream.next().await).is_some() {}
            runtime::sleep(Duration::from_millis(fastrand::u64(1..250))).await;
            finished_count.fetch_add(1, Ordering::Relaxed);
        }));
    }

    runtime::spawn({
        let swansong = swansong.clone();
        async move {
            runtime::sleep(Duration::from_millis(fastrand::u64(1..100))).await;
            swansong.shut_down();
        }
    });

    swansong.await;

    assert_eq!(expected_count, finished_count.load(Ordering::Relaxed));
}

#[test(harness)]
async fn guarded_test_coverage() {
    let swansong = Swansong::new();
    assert_eq!(swansong.guard_count(), 0);

    let future = swansong.guarded(std::future::ready("yes"));
    assert_eq!(swansong.guard_count(), 1);
    assert_eq!(future.await, "yes");
    assert_eq!(swansong.guard_count(), 0);

    let mut other_type = swansong.guarded(Vec::new());
    assert_eq!(swansong.guard_count(), 1);
    other_type.push(10);
    other_type.push(5);
    assert_eq!(other_type.first(), Some(&10));
    assert_eq!(swansong.guard_count(), 1);
    assert_eq!(other_type.into_inner(), vec![10, 5]);
    assert_eq!(swansong.guard_count(), 0);

    let stream = swansong.guarded(futures_lite::stream::repeat(10)).take(5);
    assert_eq!(swansong.guard_count(), 1);
    assert_eq!(stream.fold(0, Add::add).await, 50);
}

#[cfg(feature = "futures-io")]
#[test(harness)]
async fn futures_io() {
    use futures_lite::{
        io::{BufReader, Cursor},
        AsyncBufReadExt, AsyncReadExt, AsyncWriteExt,
    };
    let swansong = Swansong::new();

    let mut async_read = swansong.guarded(Cursor::new("hello"));
    let mut string = String::new();
    async_read.read_to_string(&mut string).await.unwrap();
    assert_eq!("hello", string);

    let input = b"hello\nworld";
    let async_buf_read = swansong.guarded(BufReader::new(&input[..]));
    assert_eq!(
        ["hello", "world"],
        async_buf_read
            .lines()
            .try_collect::<_, _, Vec<_>>()
            .await
            .unwrap()
            .as_slice(),
    );

    let mut async_write = swansong.guarded(Vec::new());
    async_write.write_all(b"hello").await.unwrap();
    assert_eq!(async_write.into_inner(), b"hello");
}

#[test]
fn iterator() {
    let swansong = Swansong::new();
    let mut iter = swansong
        .interrupt(std::iter::repeat_with(|| fastrand::u8(1..)))
        .guarded();
    assert!(iter.next().is_some());
    assert!(iter.next().is_some());
    swansong.shut_down();
    assert!(iter.next().is_none());
    assert!(iter.next().is_none());
    drop(iter);
    swansong.block_on_shutdown_completion();
}

#[test]
fn iterator_drop() {
    let swansong = Swansong::new();
    let mut iter = swansong.interrupt(std::iter::repeat_with(|| fastrand::u8(1..)));
    assert!(iter.next().is_some());
    assert!(iter.next().is_some());
    drop(swansong);
    assert!(iter.next().is_none());
    assert!(iter.next().is_none());
}

#[test]
fn iterator_size_hint() {
    let swansong = Swansong::new();
    let iter = 0..10;
    assert_eq!(iter.size_hint(), (10, Some(10)));

    let iter = swansong.interrupt(0..10);
    assert_eq!(iter.size_hint(), (0, Some(10)));
}

#[test]
fn stream_size_hint() {
    let swansong = Swansong::new();
    let stream = futures_lite::stream::iter(0..10);
    assert_eq!(stream.size_hint(), (10, Some(10)));

    let stream = swansong.interrupt(futures_lite::stream::iter(0..10));
    assert_eq!(stream.size_hint(), (0, Some(10)));
}

#[test(harness)]
async fn child_with_interrupt() {
    let parent = Swansong::new();
    let child = parent.child();

    let mut interrupt = child.interrupt(future::pending::<()>());
    assert!(poll_manually(Pin::new(&mut interrupt)).await.is_pending());

    // Parent stop propagates through child's interrupts
    parent.shut_down();
    assert_eq!(
        poll_manually(Pin::new(&mut interrupt)).await,
        Poll::Ready(None)
    );

    drop(interrupt);
    drop(child);
}

#[test(harness)]
async fn child_created_after_parent_stopped() {
    let parent = Swansong::new();
    parent.shut_down();

    let child = parent.child();
    // Child should be immediately stopped
    assert!(child.state().is_complete());

    drop(child);
    assert!(parent.state().is_complete());
}

#[test(harness)]
async fn child_multi_threaded() {
    let parent = Swansong::new();
    let finished_count = Arc::new(AtomicU8::new(0));
    let expected_count = fastrand::u8(1..10);

    for _ in 0..expected_count {
        let child = parent.child();
        let finished_count = finished_count.clone();
        runtime::spawn(async move {
            let guard = child.guard();
            runtime::sleep(Duration::from_millis(fastrand::u64(1..200))).await;
            finished_count.fetch_add(1, Ordering::Relaxed);
            drop(guard);
            child.shut_down();
        });
    }

    runtime::spawn({
        let parent = parent.clone();
        async move {
            runtime::sleep(Duration::from_millis(fastrand::u64(1..100))).await;
            parent.shut_down();
        }
    });

    parent.await;
    assert_eq!(finished_count.load(Ordering::Relaxed), expected_count);
}

#[test(harness)]
async fn shutting_down_pending_then_ready() {
    let swansong = Swansong::new();
    let mut fut = pin!(swansong.shutting_down());
    assert!(poll_manually(fut.as_mut()).await.is_pending());
    swansong.shut_down();
    assert!(poll_manually(fut.as_mut()).await.is_ready());
}

#[test(harness)]
async fn shutting_down_ready_immediately_if_already_stopped() {
    let swansong = Swansong::new();
    swansong.shut_down();
    let mut fut = pin!(swansong.shutting_down());
    assert!(poll_manually(fut.as_mut()).await.is_ready());
}

#[test(harness)]
async fn shutting_down_does_not_wait_for_guards() {
    let swansong = Swansong::new();
    let _guard = swansong.guard();
    swansong.shut_down();
    let mut fut = pin!(swansong.shutting_down());
    assert!(poll_manually(fut.as_mut()).await.is_ready());
    assert!(swansong.state().is_shutting_down());
}

#[test(harness)]
async fn shutting_down_resolves_on_parent_stop() {
    let parent = Swansong::new();
    let child = parent.child();
    let mut fut = pin!(child.shutting_down());
    assert!(poll_manually(fut.as_mut()).await.is_pending());
    parent.shut_down();
    assert!(poll_manually(fut.as_mut()).await.is_ready());
}

#[test(harness)]
async fn shutting_down_resolves_on_root_handle_drop() {
    let root = Swansong::new();
    let child = root.child();
    let mut fut = pin!(child.shutting_down());
    assert!(poll_manually(fut.as_mut()).await.is_pending());
    drop(root);
    assert!(poll_manually(fut.as_mut()).await.is_ready());
}

#[test(harness)]
async fn shutting_down_reuses_listener_across_polls() {
    // Polling a second time without a shutdown in between must reuse the
    // listener stored on the first poll. This covers poll()'s `Some(listener)`
    // reuse branch — reaching it requires the Relaxed stop check to still
    // observe false on re-entry, which only holds if shut_down hasn't been
    // called yet.
    let swansong = Swansong::new();
    let mut fut = pin!(swansong.shutting_down());
    assert!(poll_manually(fut.as_mut()).await.is_pending());
    assert!(poll_manually(fut.as_mut()).await.is_pending());
    swansong.shut_down();
    assert!(poll_manually(fut.as_mut()).await.is_ready());
}

#[test]
fn shutting_down_block_with_pre_registered_listener() {
    // Poll once to register a listener in the ShuttingDown's state, then move
    // it out (ShuttingDown: Unpin) and hand it to block(). Exercises block()'s
    // `Some(listener)` branch, which is only reachable when the caller has
    // done a prior manual poll.
    let _ = env_logger::builder().is_test(true).try_init();
    let swansong = Swansong::new();
    let mut fut = swansong.shutting_down();
    let first_poll =
        futures_lite::future::block_on(async { poll_manually(Pin::new(&mut fut)).await });
    assert!(first_poll.is_pending());

    let stopper = thread::spawn({
        let swansong = swansong.clone();
        move || {
            sleep(Duration::from_millis(fastrand::u64(1..50)));
            swansong.shut_down();
        }
    });

    fut.block();
    stopper.join().unwrap();
}

#[test]
fn shutting_down_block_sync() {
    let _ = env_logger::builder().is_test(true).try_init();
    let swansong = Swansong::new();
    let (send, receive) = std::sync::mpsc::channel();
    thread::spawn({
        let swansong = swansong.clone();
        move || {
            swansong.shutting_down().block();
            send.send(()).unwrap();
        }
    });
    thread::spawn({
        let swansong = swansong.clone();
        move || {
            sleep(Duration::from_millis(fastrand::u64(1..50)));
            swansong.shut_down();
        }
    });

    #[cfg(miri)]
    receive.recv().unwrap();
    #[cfg(not(miri))]
    receive.recv_timeout(Duration::from_secs(5)).unwrap();
}

#[test(harness)]
async fn stress_shutting_down_racing_shutdown() {
    // Race concurrent shut_down() against concurrent poll()/block() of a
    // ShuttingDown future. Intended to probabilistically exercise the SeqCst
    // re-check branches in both poll() (stopped observed true after listener
    // registration) and block() (same pattern). These branches guard the
    // narrow window between the Relaxed check and the listener registration
    // + SeqCst check.
    use std::sync::Barrier;

    #[cfg(miri)]
    let iters = 20usize;
    #[cfg(not(miri))]
    let iters = 2000usize;

    for i in 0..iters {
        let swansong = Swansong::new();
        let barrier = Arc::new(Barrier::new(2));

        let stopper = thread::spawn({
            let swansong = swansong.clone();
            let barrier = Arc::clone(&barrier);
            move || {
                barrier.wait();
                swansong.shut_down();
            }
        });

        let waiter = thread::spawn({
            let swansong = swansong.clone();
            let barrier = Arc::clone(&barrier);
            let use_block = i % 2 == 0;
            move || {
                barrier.wait();
                if use_block {
                    swansong.shutting_down().block();
                } else {
                    futures_lite::future::block_on(swansong.shutting_down());
                }
            }
        });

        stopper.join().unwrap();
        waiter.join().unwrap();
    }
}

#[test]
fn eq_and_assorted_other_conveniences() {
    let swansong = Swansong::new();
    let other = Swansong::new();
    assert_eq!(swansong, swansong.clone());
    assert_ne!(swansong, other.clone());

    let guard = swansong.guard();
    assert_eq!(guard, guard.clone());
    assert_eq!(guard, swansong.guard());
    assert_ne!(guard, other.guard());

    let guarded = swansong.guarded(String::from("hello"));
    assert_eq!(guarded, swansong.guarded(String::from("hello")));
    assert_ne!(guarded, swansong.guarded(String::from("goodbye")));
    assert_ne!(guarded, other.guarded(String::from("hello")));

    // deref
    assert_eq!(swansong.guarded("1").parse::<u8>().unwrap(), 1);

    let interrupt = swansong.interrupt(1);
    assert_eq!(interrupt, swansong.interrupt(1));
    assert_ne!(interrupt, swansong.interrupt(2));
    assert_ne!(interrupt, other.interrupt(1));

    // into inner
    let n: i32 = interrupt.into_inner();
    assert_eq!(n, 1);
}

// ============================================================================
// Specification tests — see docs/spec.md
//
// These tests encode the semantic contract in Sections 5.2–5.9 of the spec:
// guards are the only units of work, shutdown propagates downward but not
// upward, observation reports subtree state, child handle drop is transparent
// (child != root), and root handle drop implicitly shuts down.
// ============================================================================

// --- 5.2: Guards are the only units of work ---

#[test(harness)]
async fn spec_child_creation_is_transparent() {
    let parent = Swansong::new();
    let _child = parent.child();
    assert!(parent.state().is_running());
    assert_eq!(parent.guard_count(), 0);
}

#[test(harness)]
async fn spec_child_handle_drop_without_guards_is_transparent() {
    let parent = Swansong::new();
    drop(parent.child());
    assert!(parent.state().is_running());
    assert_eq!(parent.guard_count(), 0);
}

#[test(harness)]
async fn spec_live_empty_child_does_not_delay_parent_completion() {
    let parent = Swansong::new();
    let _child = parent.child();
    let mut fut = pin!(parent.clone().shut_down());
    assert!(poll_manually(fut.as_mut()).await.is_ready());
    assert!(parent.state().is_complete());
}

#[test(harness)]
async fn spec_guarded_on_discarded_child_equivalent_to_parent() {
    // `parent.child().guarded(X)` with immediately-discarded child handle
    // is semantically identical to `parent.guarded(X)`.
    let parent = Swansong::new();
    let guarded = parent.child().guarded(future::pending::<()>());
    assert_eq!(parent.guard_count(), 1);
    let mut fut = pin!(parent.clone().shut_down());
    assert!(poll_manually(fut.as_mut()).await.is_pending());
    drop(guarded);
    assert!(poll_manually(fut.as_mut()).await.is_ready());
}

#[test(harness)]
async fn spec_children_do_not_count_as_guards() {
    let parent = Swansong::new();
    let _a = parent.child();
    let _b = parent.child();
    let _c = parent.child();
    assert_eq!(parent.guard_count(), 0);
}

// --- 5.3: Propagation ---

#[test(harness)]
async fn spec_shutdown_propagates_downward() {
    let parent = Swansong::new();
    let child = parent.child();
    assert!(child.state().is_running());
    parent.shut_down();
    assert!(!child.state().is_running());
}

#[test(harness)]
async fn spec_shutdown_propagates_through_multi_level() {
    let gp = Swansong::new();
    let p = gp.child();
    let c = p.child();
    gp.shut_down();
    assert!(!p.state().is_running());
    assert!(!c.state().is_running());
}

#[test(harness)]
async fn spec_child_shutdown_does_not_affect_parent() {
    let parent = Swansong::new();
    let child = parent.child();
    child.shut_down();
    assert!(child.state().is_complete());
    assert!(parent.state().is_running());
}

#[test(harness)]
async fn spec_siblings_are_independent() {
    let parent = Swansong::new();
    let a = parent.child();
    let b = parent.child();
    a.shut_down();
    assert!(a.state().is_complete());
    assert!(b.state().is_running());
    assert!(parent.state().is_running());
}

#[test(harness)]
async fn spec_parent_completion_waits_for_subtree_guards() {
    let parent = Swansong::new();
    let child = parent.child();
    let guard = child.guard();
    let mut fut = pin!(parent.clone().shut_down());
    assert!(poll_manually(fut.as_mut()).await.is_pending());
    drop(guard);
    assert!(poll_manually(fut.as_mut()).await.is_ready());
}

// --- 5.4: Observation ---

#[test(harness)]
async fn spec_guard_count_is_subtree_sum() {
    let parent = Swansong::new();
    let child_a = parent.child();
    let child_b = parent.child();
    let grandchild = child_a.child();

    let _g1 = parent.guard();
    let _g2 = child_a.guard();
    let _g3 = child_a.guard();
    let _g4 = child_b.guard();
    let _g5 = grandchild.guard();

    assert_eq!(grandchild.guard_count(), 1);
    assert_eq!(child_a.guard_count(), 3);
    assert_eq!(child_b.guard_count(), 1);
    assert_eq!(parent.guard_count(), 5);
}

#[test(harness)]
async fn spec_parent_state_reflects_subtree() {
    let parent = Swansong::new();
    let child = parent.child();
    let _g = child.guard();
    parent.shut_down();
    // Guard in child's subtree prevents parent from reaching Complete.
    assert!(parent.state().is_shutting_down());
}

// --- 5.7: Edge cases ---

#[test(harness)]
async fn spec_guard_on_stopped_swansong_delays_completion() {
    let swansong = Swansong::new();
    swansong.shut_down();
    let guard = swansong.guard();
    let mut fut = pin!(swansong.clone().into_future());
    assert!(poll_manually(fut.as_mut()).await.is_pending());
    drop(guard);
    assert!(poll_manually(fut.as_mut()).await.is_ready());
}

#[test(harness)]
async fn spec_guard_outlives_child_handle() {
    // A Guard created on a child, held past the drop of every child Swansong
    // clone, must still contribute to the parent's subtree accounting.
    let parent = Swansong::new();
    let guard = {
        let child = parent.child();
        child.guard()
    };
    assert_eq!(parent.guard_count(), 1);
    let mut fut = pin!(parent.clone().shut_down());
    assert!(poll_manually(fut.as_mut()).await.is_pending());
    drop(guard);
    assert!(poll_manually(fut.as_mut()).await.is_ready());
}

#[test(harness)]
async fn spec_interrupt_on_pinned_child_delegates_after_handle_drop() {
    // If something (here, a Guard) pins the child's Inner, the child stays
    // Running after its handle drops. An Interrupt on the child delegates
    // to the wrapped type rather than terminating.
    let parent = Swansong::new();
    let (_guard, mut interrupt) = {
        let child = parent.child();
        (child.guard(), child.interrupt(future::pending::<()>()))
    };
    assert!(poll_manually(Pin::new(&mut interrupt)).await.is_pending());
}

#[test(harness)]
async fn spec_interrupt_terminates_when_child_collected() {
    // With no Guard and no ShutdownCompletion, the child's Inner is
    // collected once its last Swansong clone drops. Interrupts on it return
    // terminal via Weak::upgrade failing — no indefinite poll.
    let parent = Swansong::new();
    let mut interrupt = {
        let child = parent.child();
        child.interrupt(future::pending::<()>())
    };
    assert_eq!(
        poll_manually(Pin::new(&mut interrupt)).await,
        Poll::Ready(None)
    );
}

// --- 5.8: Child handle-drop does not signal shutdown ---

#[test(harness)]
async fn spec_child_handle_drop_does_not_stop_the_child() {
    // Dropping the last clone of a non-root Swansong must not signal
    // shutdown. The parent is still Running; the outstanding Guard still
    // counts in the parent's subtree total.
    let parent = Swansong::new();
    let _guard = {
        let child = parent.child();
        child.guard()
    };
    assert!(parent.state().is_running());
    assert_eq!(parent.guard_count(), 1);
}

// --- 5.9: Root drop propagates downward ---

#[test(harness)]
async fn spec_root_drop_propagates_downward() {
    let root = Swansong::new();
    let child = root.child();
    let grandchild = child.child();
    drop(root);
    assert!(!child.state().is_running());
    assert!(!grandchild.state().is_running());
}

// ============================================================================
// Stress tests — exercise the transition-propagation logic in inner.rs under
// real thread interleavings. Most valuable under miri and the thread-sanitizer
// CI job, where subtle races have a chance to surface.
// ============================================================================

#[test(harness)]
async fn stress_racing_guard_transitions_on_node() {
    // Hammer a single node with concurrent create/drop cycles. Exercises the
    // empty↔nonempty transition detection and the ancestor walk under
    // contention. If transitions are mis-attributed, the parent's subtree
    // count will drift from zero.
    let parent = Swansong::new();
    let child = parent.child();

    #[cfg(miri)]
    let (iters, workers) = (3usize, 2usize);
    #[cfg(not(miri))]
    let (iters, workers) = (500usize, 4usize);

    let mut threads = vec![];
    for _ in 0..workers {
        let child = child.clone();
        threads.push(thread::spawn(move || {
            for _ in 0..iters {
                let g = child.guard();
                drop(g);
            }
        }));
    }
    for t in threads {
        t.join().unwrap();
    }

    assert_eq!(child.guard_count(), 0);
    assert_eq!(parent.guard_count(), 0);
    assert!(child.state().is_running());
    assert!(parent.state().is_running());
}

#[test(harness)]
async fn stress_sibling_guards_common_ancestor() {
    // Many workers each create and drop guards on their own sibling child of
    // a shared parent. Exercises concurrent ancestor-walks that meet at the
    // parent's nonempty_kids counter.
    let parent = Swansong::new();

    #[cfg(miri)]
    let (iters, siblings) = (2usize, 3usize);
    #[cfg(not(miri))]
    let (iters, siblings) = (200usize, 6usize);

    let mut threads = vec![];
    for _ in 0..siblings {
        let child = parent.child();
        threads.push(thread::spawn(move || {
            for _ in 0..iters {
                let g = child.guard();
                drop(g);
            }
            child
        }));
    }
    let children: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();

    assert_eq!(parent.guard_count(), 0);
    for child in &children {
        assert_eq!(child.guard_count(), 0);
        assert!(child.state().is_running());
    }
    assert!(parent.state().is_running());
}

#[test(harness)]
async fn stress_deep_tree_concurrent_guards() {
    // Root → middle → leaf tree, with workers holding guards at different
    // depths. Exercises multi-level ancestor walks under contention.
    let root = Swansong::new();
    let mid = root.child();
    let leaf = mid.child();

    #[cfg(miri)]
    let (iters, workers) = (2usize, 2usize);
    #[cfg(not(miri))]
    let (iters, workers) = (200usize, 4usize);

    let mut threads = vec![];
    for _ in 0..workers {
        let leaf = leaf.clone();
        let mid = mid.clone();
        let root = root.clone();
        threads.push(thread::spawn(move || {
            for _ in 0..iters {
                let g_leaf = leaf.guard();
                let g_mid = mid.guard();
                let g_root = root.guard();
                drop(g_leaf);
                drop(g_mid);
                drop(g_root);
            }
        }));
    }
    for t in threads {
        t.join().unwrap();
    }

    assert_eq!(leaf.guard_count(), 0);
    assert_eq!(mid.guard_count(), 0);
    assert_eq!(root.guard_count(), 0);
}

#[test(harness)]
async fn stress_child_creation_racing_shutdown() {
    // Create children from multiple workers while another thread initiates
    // parent shutdown. Every child produced must either be stopped
    // (propagation reached it) or created-already-stopped. No child may
    // remain Running after the shutdown completes.
    let parent = Swansong::new();

    #[cfg(miri)]
    let (iters, creators) = (3usize, 2usize);
    #[cfg(not(miri))]
    let (iters, creators) = (40usize, 4usize);

    let children: Arc<Mutex<Vec<Swansong>>> = Arc::new(Mutex::new(vec![]));
    let started = Arc::new(AtomicUsize::new(0));

    let mut threads = vec![];
    for _ in 0..creators {
        let parent = parent.clone();
        let children = Arc::clone(&children);
        let started = Arc::clone(&started);
        threads.push(thread::spawn(move || {
            started.fetch_add(1, Ordering::Relaxed);
            for _ in 0..iters {
                let child = parent.child();
                children.lock().unwrap().push(child);
            }
        }));
    }

    threads.push(thread::spawn({
        let parent = parent.clone();
        let started = Arc::clone(&started);
        move || {
            // Let creators get started before pulling the trigger
            while started.load(Ordering::Relaxed) < creators {
                thread::yield_now();
            }
            sleep(Duration::from_millis(fastrand::u64(0..10)));
            parent.shut_down();
        }
    }));

    for t in threads {
        t.join().unwrap();
    }

    let children = children.lock().unwrap();
    for child in children.iter() {
        assert!(
            !child.state().is_running(),
            "child still Running after parent shutdown; state={:?}",
            child.state()
        );
    }
    assert!(parent.state().is_complete());
}
