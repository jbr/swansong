use futures_lite::{StreamExt, stream::unfold};

fn main() {
    use async_global_executor as executor;
    use async_io::Timer;
    use std::{future::pending, pin::pin, time::Duration};
    use swansong::Swansong;
    futures_lite::future::block_on(async {
        let swansong = Swansong::new();
        executor::spawn(swansong.interrupt(pending::<()>()).guarded()).detach();
        executor::spawn({
            let swansong = swansong.clone();
            async move {
                let mut stream = pin!(swansong.interrupt(unfold((), |()| async {
                    Timer::after(Duration::from_millis(100)).await;
                    Some((fastrand::u8(..), ()))
                })));

                while let Some(n) = stream.next().await {
                    println!("{n}");
                }
            }
        })
        .detach();
        executor::spawn(swansong.guarded(Timer::after(Duration::from_secs(1)))).detach();
        swansong.shut_down().await;
    });
}
