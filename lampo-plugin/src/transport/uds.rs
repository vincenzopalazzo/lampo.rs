//! Unix-socket connector and listener for a local gRPC plugin.
//!
//! The daemon spawns the plugin and passes `--lampo-socket`. The plugin
//! listens there. HTTP/2 runs on that socket with no TLS: the socket mode
//! is `0600` and it lives in the node directory.
//!
//! The client connector clones. A second `HandleRpc` does not wait on the
//! mutex that used to cover the whole call.
#![cfg(feature = "grpc")]

use std::pin::Pin;
use std::task::{Context, Poll};

use http::Uri;
use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;
use tower::Service;

/// Connects every gRPC call to the same plugin socket.
#[derive(Clone, Debug)]
pub struct UnixConnector {
    path: std::path::PathBuf,
}

impl UnixConnector {
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl Service<Uri> for UnixConnector {
    type Response = TokioIo<UnixStream>;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Future =
        Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _uri: Uri) -> Self::Future {
        let path = self.path.clone();
        Box::pin(async move {
            let stream = UnixStream::connect(path).await?;
            Ok(TokioIo::new(stream))
        })
    }
}

/// `UnixStream` does not implement tonic's `Connected`. Wrap it.
pub struct UnixIo {
    inner: UnixStream,
}

impl UnixIo {
    pub fn new(stream: UnixStream) -> Self {
        Self { inner: stream }
    }
}

impl tonic::transport::server::Connected for UnixIo {
    type ConnectInfo = ();

    fn connect_info(&self) -> Self::ConnectInfo {}
}

impl tokio::io::AsyncRead for UnixIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl tokio::io::AsyncWrite for UnixIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Accept loop tonic can serve. One connection at a time is not enough:
/// `foo` calls `yoooo` on a second connection while the first is still open.
pub struct UnixIncoming {
    listener: tokio::net::UnixListener,
}

impl UnixIncoming {
    pub fn bind(path: &std::path::Path) -> std::io::Result<Self> {
        if path.exists() {
            let _ = std::fs::remove_file(path);
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let listener = tokio::net::UnixListener::bind(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self { listener })
    }
}

impl futures_core::Stream for UnixIncoming {
    type Item = Result<UnixIo, std::io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.listener.poll_accept(cx) {
            Poll::Ready(Ok((stream, _))) => Poll::Ready(Some(Ok(UnixIo::new(stream)))),
            Poll::Ready(Err(err)) => Poll::Ready(Some(Err(err))),
            Poll::Pending => Poll::Pending,
        }
    }
}
