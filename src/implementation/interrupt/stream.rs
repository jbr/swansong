use super::Interrupt;
use futures_core::Stream;
use std::{
    pin::Pin,
    task::{Context, Poll},
};

impl<T: Stream> Stream for Interrupt<T> {
    type Item = T::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        if this.stop_listener.is_stopped() {
            log::trace!("stopped, cancelling");
            return Poll::Ready(None);
        }

        match this.wrapped_type.poll_next(cx) {
            Poll::Ready(item) => Poll::Ready(item),
            Poll::Pending => this.stop_listener.poll_stopped(cx).map(|()| None),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, self.wrapped_type.size_hint().1)
    }
}
