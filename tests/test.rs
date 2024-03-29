use async_global_executor::spawn;
use async_io::{block_on, Timer};
use futures_lite::{
    io::{BufReader, Cursor},
    AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, FutureExt, Stream, StreamExt,
};
use std::{
    future::{self, Future, IntoFuture},
    ops::Add,
    pin::{pin, Pin},
    process::Termination,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
    task::{Context, Poll},
    thread::{self, sleep},
    time::Duration,
};
use swansong::Swansong;
use test_harness::test;

fn harness<F, Fut, O>(test: F) -> O
where
    F: FnOnce() -> Fut,
    O: Termination,
    Fut: Future<Output = O> + Send,
{
    let _ = env_logger::builder().is_test(true).try_init();
    block_on(
        async {
            Timer::after(Duration::from_secs(5)).await;
            None
        }
        .or(async move { Some(test().await) }),
    )
    .expect("timed out")
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
    assert!(poll_manually(future.as_mut()).await.is_pending());
    swansong.stop();
    assert!(poll_manually(future.as_mut()).await.is_pending());
    drop(guard);
    assert!(poll_manually(future.as_mut()).await.is_pending());
    drop(guard2);
    assert!(poll_manually(future.as_mut()).await.is_ready());
}

#[test(harness)]
async fn multi_threaded() {
    let swansong = Swansong::new();
    let finished_count = Arc::new(AtomicU8::new(0));
    let expected_count = fastrand::u8(..);

    for _ in 0..expected_count {
        let guard = swansong.guard();
        let finished_count = finished_count.clone();
        thread::spawn(move || {
            let _guard = guard;
            sleep(Duration::from_millis(fastrand::u64(..500)));
            finished_count.fetch_add(1, Ordering::Relaxed);
        });
    }

    thread::spawn({
        let swansong = swansong.clone();
        move || {
            sleep(Duration::from_millis(fastrand::u64(..500)));
            swansong.stop();
        }
    });

    swansong.await;

    assert_eq!(finished_count.load(Ordering::Relaxed), expected_count);
}

#[test]
fn multi_threaded_blocking() {
    let _ = env_logger::builder().is_test(true).try_init();
    let swansong = Swansong::new();
    let finished_count = Arc::new(AtomicU8::new(0));
    let expected_count = fastrand::u8(..);

    for _ in 0..expected_count {
        let guard = swansong.guard();
        let finished_count = finished_count.clone();
        thread::spawn(move || {
            let _guard = guard;
            sleep(Duration::from_millis(fastrand::u64(..500)));
            finished_count.fetch_add(1, Ordering::Relaxed);
        });
    }

    thread::spawn({
        let swansong = swansong.clone();
        move || {
            sleep(Duration::from_millis(fastrand::u64(..500)));
            swansong.stop();
        }
    });

    let (send, receive) = std::sync::mpsc::channel();
    thread::spawn(move || {
        swansong.block();
        send.send(()).unwrap();
    });

    receive.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(finished_count.load(Ordering::Relaxed), expected_count);
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
    let mut future = swansong.stop_future(Fut(false));
    assert!(poll_manually(Pin::new(&mut future)).await.is_pending());
    swansong.stop();
    assert_eq!(
        poll_manually(Pin::new(&mut future)).await,
        Poll::Ready(None)
    );

    let swansong = Swansong::new();
    let mut future = swansong.stop_future(Fut(false));
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
    let mut stream = swansong.stop_stream(Stream_(false));
    assert_eq!(stream.next().await, Some(()));
    assert_eq!(stream.next().await, Some(()));
    swansong.stop();
    assert_eq!(stream.next().await, None);

    let swansong = Swansong::new();
    let mut stream = swansong.stop_stream(Stream_(false));
    assert_eq!(stream.next().await, Some(()));
    assert_eq!(stream.next().await, Some(()));
    stream.ready();
    assert_eq!(stream.next().await, None);
    swansong.stop();
    assert_eq!(stream.next().await, None);
}

#[test(harness)]
async fn multi_threaded_future_guarded() {
    let swansong = Swansong::new();
    let canceled_count = Arc::new(AtomicU8::new(0));
    let finished_count = Arc::new(AtomicU8::new(0));
    let expected_count = fastrand::u8(..);

    for _ in 0..expected_count {
        let finished_count = finished_count.clone();
        let canceled_count = canceled_count.clone();
        let fut = swansong
            .stop_future(async move {
                for _ in 0..fastrand::u8(..5) {
                    Timer::after(Duration::from_millis(fastrand::u64(..100))).await;
                }
                finished_count.fetch_add(1, Ordering::Relaxed);
            })
            .guarded();

        spawn(swansong.guarded(async move {
            let res = fut.await;
            Timer::interval(Duration::from_millis(fastrand::u64(0..250))).await;
            if res.is_none() {
                canceled_count.fetch_add(1, Ordering::Relaxed);
            }
        }))
        .detach();
    }

    spawn({
        let swansong = swansong.clone();
        async move {
            Timer::after(Duration::from_millis(fastrand::u64(..500))).await;
            swansong.stop();
        }
    })
    .detach();

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
    let expected_count = fastrand::u8(..);

    for _ in 0..expected_count {
        let finished_count = finished_count.clone();
        let mut stream = swansong
            .stop_stream(Timer::interval(Duration::from_millis(fastrand::u64(
                0..100,
            ))))
            .guarded();

        spawn(swansong.guarded(async move {
            while (stream.next().await).is_some() {}
            Timer::interval(Duration::from_millis(fastrand::u64(0..250))).await;
            finished_count.fetch_add(1, Ordering::Relaxed);
        }))
        .detach();
    }

    spawn({
        let swansong = swansong.clone();
        async move {
            Timer::after(Duration::from_millis(fastrand::u64(..500))).await;
            swansong.stop();
        }
    })
    .detach();

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
    assert_eq!(async_write, b"hello");
}
