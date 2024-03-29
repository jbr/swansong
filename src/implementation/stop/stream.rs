use super::Stop;
use futures_lite::Stream;
use std::{
    future::Future,
    pin::Pin,
    task::{ready, Context, Poll},
};

impl<T: Stream> Stream for Stop<T> {
    type Item = T::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        let Some(inner) = this.inner.upgrade() else {
            log::trace!("failed to upgrade weak");
            return Poll::Ready(None);
        };
        let stop_listener = this.stop_listener;
        let mut stream = this.wrapped_type;

        loop {
            if inner.is_stopped_relaxed() {
                return Poll::Ready(None);
            }

            let listener = match stop_listener {
                Some(listener) => listener,
                None => {
                    let listener = stop_listener.insert(inner.listen_stop());
                    if inner.is_stopped() {
                        return Poll::Ready(None);
                    }
                    listener
                }
            };

            match stream.as_mut().poll_next(cx) {
                Poll::Ready(item) => {
                    return Poll::Ready(item);
                }

                Poll::Pending => {
                    if inner.is_stopped_relaxed() {
                        return Poll::Ready(None);
                    }
                    ready!(Pin::new(listener).poll(cx));
                    *stop_listener = None;
                }
            };
        }
    }
}
