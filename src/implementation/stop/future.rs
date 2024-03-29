use super::Stop;
use std::{
    future::Future,
    pin::Pin,
    task::{ready, Context, Poll},
};

impl<T: Future> Future for Stop<T> {
    type Output = Option<T::Output>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let stop_listener = this.stop_listener;
        let Some(inner) = this.inner.upgrade() else {
            log::trace!("unable to upgrade, cancelling");
            return Poll::Ready(None);
        };

        let mut future = this.wrapped_type;
        loop {
            log::trace!("top of loop");
            if inner.is_stopped_relaxed() {
                log::trace!("stopped, cancelling");
                return Poll::Ready(None);
            }

            let listener = match stop_listener {
                Some(listener) => listener,
                None => {
                    log::trace!("registering new stop_listener");
                    let listener = stop_listener.insert(inner.listen_stop());
                    if inner.is_stopped() {
                        log::trace!("stopped, cancelling");
                        return Poll::Ready(None);
                    }
                    listener
                }
            };

            match future.as_mut().poll(cx) {
                Poll::Ready(result) => {
                    log::trace!("future was ready");
                    *stop_listener = None;
                    return Poll::Ready(Some(result));
                }

                Poll::Pending => {
                    if inner.is_stopped_relaxed() {
                        return Poll::Ready(None);
                    }
                    ready!(Pin::new(listener).poll(cx));
                    *stop_listener = None;
                }
            }
        }
    }
}
