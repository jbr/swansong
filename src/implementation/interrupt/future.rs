use super::Interrupt;
use std::{
    future::Future,
    pin::Pin,
    task::{ready, Context, Poll},
};

impl<T: Future> Future for Interrupt<T> {
    type Output = Option<T::Output>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();
        loop {
            log::trace!("top of loop");
            if this.inner.is_stopped_relaxed() {
                log::trace!("stopped, cancelling");
                return Poll::Ready(None);
            }

            let Some(listener) = this.stop_listener.listen(this.inner) else {
                return Poll::Ready(None);
            };

            match this.wrapped_type.as_mut().poll(cx) {
                Poll::Ready(result) => {
                    log::trace!("future was ready");
                    return Poll::Ready(Some(result));
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
}
