use super::Interrupt;
use futures_core::Stream;
use std::{
    future::Future,
    pin::Pin,
    task::{ready, Context, Poll},
};

impl<T: Stream> Stream for Interrupt<T> {
    type Item = T::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        loop {
            if this.inner.is_stopped_relaxed() {
                log::trace!("stopped, cancelling");
                return Poll::Ready(None);
            }

            let Some(listener) = this.stop_listener.listen(this.inner) else {
                return Poll::Ready(None);
            };

            match this.wrapped_type.as_mut().poll_next(cx) {
                Poll::Ready(item) => {
                    return Poll::Ready(item);
                }

                Poll::Pending => {
                    if this.inner.is_stopped_relaxed() {
                        log::trace!("stopped, cancelling");
                        return Poll::Ready(None);
                    }

                    ready!(Pin::new(listener).poll(cx));
                    **this.stop_listener = None;
                }
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, self.wrapped_type.size_hint().1)
    }
}
