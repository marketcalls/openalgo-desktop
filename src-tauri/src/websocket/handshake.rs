//! A WebSocket server's handshake check, written as a closure.
//!
//! tungstenite's `Callback` refuses with its own `ErrorResponse`, an HTTP
//! response too large to return in a closure's `Result` (clippy
//! `result_large_err` checks closures and leaves trait methods alone). A
//! check returns its refusal boxed, as the HTTP routes' `EarlyReply` does,
//! and [`Check`] unboxes it where tungstenite takes it.

use tokio_tungstenite::tungstenite::handshake::server::{
    Callback, ErrorResponse, Request, Response,
};

/// A refused handshake: the HTTP response the client gets, boxed.
pub type Refusal = Box<ErrorResponse>;

/// The tungstenite callback for a handshake check, built by [`check`].
pub struct Check<F>(F);

/// The handshake callback for `f`: `Ok` accepts with that response, `Err`
/// refuses with that one.
pub fn check<F>(f: F) -> Check<F>
where
    F: FnOnce(&Request, Response) -> Result<Response, Refusal>,
{
    Check(f)
}

impl<F> Callback for Check<F>
where
    F: FnOnce(&Request, Response) -> Result<Response, Refusal>,
{
    fn on_request(self, req: &Request, resp: Response) -> Result<Response, ErrorResponse> {
        (self.0)(req, resp).map_err(|r| *r)
    }
}
