//! The process-survival seam: one `tower::Service` wrapper that turns a panic
//! in ANY request/notification handler into a logged warning + a graceful LSP
//! response, so no single request can take the server down.
//!
//! Why a service layer and not per-handler `catch_unwind`: tower-lsp 0.20 drives
//! every handler future on ONE task. `Server::serve` reads a message, calls
//! `service.call(req)` to get a future, and hands that future to a
//! `buffer_unordered` stream inside a `join!` — all polled on the same task that
//! runs `serve().await`. A panic while polling any handler future therefore
//! unwinds `buffer_unordered` → `join!` → the whole `serve` task, killing every
//! in-flight and future request (and, since `serve` is awaited from `main`, the
//! process). Wrapping `call`'s future in `catch_unwind` here — at the single
//! choke point every request and notification flows through — makes the panic
//! boundary a property of the boundary itself (rule #10): every handler inherits
//! it by construction, present and future, with no per-handler ceremony.

use std::panic::AssertUnwindSafe;
use std::task::{Context, Poll};

use futures::future::{BoxFuture, FutureExt};
use tower::Service;
use tower_lsp::jsonrpc::{Error, Request, Response};
use tower_lsp::lsp_types::notification::{Exit, Notification};

use crate::lsp::stdio_bridge::SessionEnd;

/// Wraps an `LspService` (`Service<Request, Response = Option<Response>>`) so a
/// panicking handler degrades instead of unwinding the server loop.
pub struct PanicGuard<S> {
    inner: S,
    session_end: SessionEnd,
}

impl<S> PanicGuard<S> {
    /// `session_end` is signalled when the client's `exit` passes through: the
    /// one place every message is seen, and tower-lsp consumes `exit` itself
    /// without calling a handler.
    pub fn new(inner: S, session_end: SessionEnd) -> Self {
        Self { inner, session_end }
    }
}

impl<S> Service<Request> for PanicGuard<S>
where
    S: Service<Request, Response = Option<Response>>,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
{
    type Response = Option<Response>;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Option<Response>, S::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let method = req.method().to_string();
        // A request carries an id and the client is blocked awaiting its reply;
        // a notification has none and expects no response. On panic we must send
        // an error response for the former (or the client hangs) and nothing for
        // the latter.
        let id = req.id().cloned();
        let fut = self.inner.call(req);
        if method == Exit::METHOD {
            self.session_end.signal();
        }
        Box::pin(async move {
            match AssertUnwindSafe(fut).catch_unwind().await {
                Ok(result) => result,
                Err(_) => {
                    log::warn!(
                        "handler for `{method}` panicked; degraded to an error/empty response \
                         (server stays up)"
                    );
                    Ok(id.map(|id| Response::from_error(id, Error::internal_error())))
                }
            }
        })
    }
}

#[cfg(test)]
#[path = "panic_guard_tests.rs"]
mod tests;
