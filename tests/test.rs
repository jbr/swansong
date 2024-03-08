use futures_lite::future::block_on;
use std::{
    future::{Future, IntoFuture},
    pin::{pin, Pin},
    process::Termination,
    task::Poll,
    thread::{sleep, spawn},
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
    block_on(test())
}

async fn poll_manually<'a, F: Future>(future: &mut Pin<&mut F>) -> Poll<F::Output> {
    std::future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await
}

#[test(harness)]
async fn swansong() {
    let swansong = Swansong::new();
    let mut future = pin!(swansong.clone().into_future());

    assert!(poll_manually(&mut future).await.is_pending());
    let guard = swansong.guard();
    assert!(poll_manually(&mut future).await.is_pending());
    swansong.stop();
    assert!(poll_manually(&mut future).await.is_pending());
    drop(guard);
    assert!(poll_manually(&mut future).await.is_ready());
}

#[test(harness)]
async fn multi_threaded() {
    let swansong = Swansong::new();

    for _ in 0..fastrand::u8(..) {
        let guard = swansong.guard();
        spawn(move || {
            let _guard = guard;
            sleep(Duration::from_millis(fastrand::u64(..500)));
        });
    }

    spawn({
        let swansong = swansong.clone();
        move || {
            sleep(Duration::from_millis(fastrand::u64(..500)));
            swansong.stop();
        }
    });

    swansong.await
}
