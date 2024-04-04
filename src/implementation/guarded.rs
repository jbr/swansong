use super::Inner;
use crate::Guard;
use futures_core::Stream;
use futures_io::{AsyncBufRead, AsyncRead, AsyncWrite};
use std::{
    future::Future,
    ops::{Deref, DerefMut},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

pin_project_lite::pin_project! {
    /// Guarded is a convenient way to attach a [`Guard`] to another type.
    ///
    /// Guarded does not stop the wrapped type on shutdown, but will delay shutdown until it is
    /// dropped. To stop the wrapped type, use
    /// [`Swansong::stop_future`][crate::Swansong::stop_future] or
    /// [`Swansong::stop_stream`][crate::Swansong::stop_stream]. To both stop the wrapped type and
    /// also act as a guard, use [`Stop::guarded`][crate::Stop::Guarded].
    ///
    /// Guarded implements Future, Stream, Clone, Debug, AsyncRead, and AsyncWrite when the wrapped
    /// type also does.
    ///
    /// Guarded implements Deref and DerefMut to the wrapped type.
    #[derive(Clone, Debug)]
    pub struct Guarded<T> {
        guard: Guard,
        #[pin]
        wrapped_type: T
    }
}

impl<T> Guarded<T> {
    pub(crate) fn new(inner: &Arc<Inner>, wrapped_type: T) -> Self {
        Self {
            guard: Guard::new(inner),
            wrapped_type,
        }
    }

    /// Transform this `Guarded<T>` into the inner `T`, dropping the [`Guard`] in the process.
    ///
    /// Doing this allows shutdown to proceed if no other guards exist and shutdown is initiated.
    pub fn into_inner(self) -> T {
        self.wrapped_type
    }
}

impl<T: Future> Future for Guarded<T> {
    type Output = T::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.project().wrapped_type.poll(cx)
    }
}

impl<T: Stream> Stream for Guarded<T> {
    type Item = T::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.project().wrapped_type.poll_next(cx)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.wrapped_type.size_hint()
    }
}

impl<T> Deref for Guarded<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.wrapped_type
    }
}

impl<T> DerefMut for Guarded<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.wrapped_type
    }
}

impl<T: AsyncRead> AsyncRead for Guarded<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        self.project().wrapped_type.poll_read(cx, buf)
    }

    fn poll_read_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &mut [std::io::IoSliceMut<'_>],
    ) -> Poll<std::io::Result<usize>> {
        self.project().wrapped_type.poll_read_vectored(cx, bufs)
    }
}

impl<T: AsyncWrite> AsyncWrite for Guarded<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.project().wrapped_type.poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.project().wrapped_type.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.project().wrapped_type.poll_close(cx)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        self.project().wrapped_type.poll_write_vectored(cx, bufs)
    }
}

impl<T: AsyncBufRead> AsyncBufRead for Guarded<T> {
    fn poll_fill_buf(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<&[u8]>> {
        self.project().wrapped_type.poll_fill_buf(cx)
    }

    fn consume(self: Pin<&mut Self>, amt: usize) {
        self.project().wrapped_type.consume(amt);
    }
}

impl<T, U> PartialEq<U> for Guarded<T>
where
    T: PartialEq<U>,
{
    fn eq(&self, other: &U) -> bool {
        self.wrapped_type.eq(other)
    }
}
