use crate::Interrupt;
use std::{
    io::Result,
    pin::Pin,
    task::{Context, Poll},
};

#[cfg(feature = "futures-io")]
impl<T: futures_io::AsyncRead> futures_io::AsyncRead for Interrupt<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<Result<usize>> {
        if self.is_stopped_relaxed() {
            Poll::Ready(Ok(0))
        } else {
            self.project().wrapped_type.poll_read(cx, buf)
        }
    }

    fn poll_read_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &mut [std::io::IoSliceMut<'_>],
    ) -> Poll<Result<usize>> {
        if self.is_stopped_relaxed() {
            Poll::Ready(Ok(0))
        } else {
            self.project().wrapped_type.poll_read_vectored(cx, bufs)
        }
    }
}

#[cfg(feature = "tokio")]
impl<T: tokio::io::AsyncRead> tokio::io::AsyncRead for Interrupt<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<Result<()>> {
        if self.is_stopped_relaxed() {
            Poll::Ready(Ok(()))
        } else {
            self.project().wrapped_type.poll_read(cx, buf)
        }
    }
}
