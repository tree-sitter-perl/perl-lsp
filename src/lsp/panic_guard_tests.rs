//! Proves the two process-survival guarantees: (1) a panic in any handler
//! degrades to a graceful response and the service stays usable, and (2) the
//! detached-task shape used for `window/workDoneProgress/create` keeps the
//! reply receiver alive past the timeout — the exact drop that panicked #36.

use std::task::{Context, Poll};

use futures::future::BoxFuture;
use tower::Service;
use tower_lsp::jsonrpc::{Request, Response};
use tower_lsp::ExitedError;

use super::PanicGuard;
use crate::lsp::stdio_bridge::SessionEnd;

/// Stand-in `LspService`: the `boom` method panics mid-poll (like a real
/// handler hitting an `unwrap`), everything else answers `Ok`.
struct MockService;

impl Service<Request> for MockService {
    type Response = Option<Response>;
    type Error = ExitedError;
    type Future = BoxFuture<'static, Result<Option<Response>, ExitedError>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let method = req.method().to_string();
        let id = req.id().cloned();
        Box::pin(async move {
            if method == "boom" {
                panic!("simulated handler panic");
            }
            Ok(id.map(|id| Response::from_ok(id, serde_json::json!("ok"))))
        })
    }
}

#[tokio::test]
async fn handler_panic_degrades_and_service_survives() {
    // Silence the default panic hook's backtrace during the intentional panic
    // so the test output stays clean; restore it afterwards.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));

    let mut guard = PanicGuard::new(MockService, SessionEnd::default());

    // A panicking REQUEST (has id): the client is blocked on a reply, so the
    // guard must yield an error response — never unwind the server task.
    let boom = Request::build("boom").id(1).finish();
    let resp = guard
        .call(boom)
        .await
        .expect("panic is caught -> Ok, not a propagated unwind")
        .expect("a request carries an id, so it gets a response object");
    assert!(resp.is_error(), "panicked request degrades to an error response");

    // The service is still fully usable after the panic.
    let ok = Request::build("hover").id(2).finish();
    let resp2 = guard
        .call(ok)
        .await
        .expect("Ok")
        .expect("response present");
    assert!(resp2.is_ok(), "later requests succeed — the server stayed up");

    // A panicking NOTIFICATION (no id) degrades to no response, not a crash.
    let boom_notif = Request::build("boom").finish();
    let resp3 = guard.call(boom_notif).await.expect("Ok");
    assert!(resp3.is_none(), "a notification never emits a response");

    std::panic::set_hook(prev);
}

/// `exit` ends the session the moment it passes the guard, whatever the
/// handlers still in flight are waiting for; no other message does.
#[tokio::test]
async fn exit_signals_the_session_end() {
    use std::time::Duration;

    let end = SessionEnd::default();
    let mut guard = PanicGuard::new(MockService, end.clone());

    let _ = guard.call(Request::build("shutdown").id(1).finish()).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(20), end.ended()).await.is_err(),
        "shutdown alone leaves the session open"
    );

    let _ = guard.call(Request::build("exit").finish());
    assert!(
        tokio::time::timeout(Duration::from_secs(5), end.ended()).await.is_ok(),
        "exit ends the session without its future being polled"
    );
}

/// #36: tower-lsp keeps a server→client request's oneshot SENDER in its pending
/// map until the reply lands, and does `tx.send(reply).expect("receiver already
/// dropped")`. If the reply arrives after we dropped the RECEIVER, that
/// `expect` panics the message-routing loop. The fix spawns the request on a
/// DETACHED task so the timeout drops only the `JoinHandle`, not the task —
/// keeping the receiver alive for a late reply.
#[tokio::test]
async fn detached_request_keeps_receiver_alive_past_timeout() {
    use futures::channel::oneshot;
    use std::time::Duration;

    // The fix's shape: the reply-awaiting future (owning `rx`) is spawned; the
    // 2s cap only drops the JoinHandle.
    let (tx, rx) = oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        let _ = rx.await; // parks until the (late) client reply
    });
    // Timeout elapses; the JoinHandle is dropped — but dropping a JoinHandle
    // does NOT abort the task, so `rx` stays alive inside it.
    let _ = tokio::time::timeout(Duration::from_millis(20), handle).await;
    tokio::time::sleep(Duration::from_millis(5)).await;
    // The late client reply: with the receiver still alive, tower-lsp's
    // `tx.send(reply).expect(...)` sees `Ok` — no panic.
    assert!(
        tx.send(()).is_ok(),
        "detached task keeps the receiver alive; a late reply routes cleanly"
    );

    // Contrast — the OLD shape awaited the receiver directly under the timeout,
    // so the timeout DROPPED it; a late send then fails, and tower-lsp turns
    // that `Err` into the #36 panic.
    let (tx2, rx2) = oneshot::channel::<()>();
    let _ = tokio::time::timeout(Duration::from_millis(20), rx2).await;
    assert!(
        tx2.send(()).is_err(),
        "a direct-await timeout drops the receiver — this is the #36 panic path"
    );
}
