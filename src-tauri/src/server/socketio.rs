//! Socket.IO on the main port (`/socket.io`), like Flask-SocketIO on the
//! web. Only a browser with a signed-in session may stay connected; the
//! subscribers push the web's event names through `SocketEmitter`.

use crate::server::middleware::cookie_value;
use crate::session::web::COOKIE_NAME;
use crate::state::AppState;
use socketioxide::extract::{SocketRef, State};
use socketioxide::layer::SocketIoLayer;
use socketioxide::SocketIo;
use std::sync::{Arc, Weak};

/// A connection is accepted only for a signed-in browser session. The
/// socket is already in the namespace when this runs, so a sign-out that
/// ends the session after this check finds it in its sweep
/// ([`disconnect_signed_out`]), and every push re-checks the session
/// (`SocketEmitter::emit_signed_in`): a connection racing a sign-out ends
/// closed either way (security review S-09).
fn on_connect(socket: SocketRef, State(app): State<Weak<AppState>>) {
    let Some(ctx) = app.upgrade() else {
        let _ = socket.disconnect();
        return;
    };
    if !signed_in(&ctx, &socket) {
        let _ = socket.disconnect();
        return;
    }
    crate::server::routes::strategy_module::register_socket_handlers(&socket);
}

/// Whether the browser session behind a connection is still signed in.
pub fn signed_in(ctx: &AppState, socket: &SocketRef) -> bool {
    cookie_value(&socket.req_parts().headers, COOKIE_NAME)
        .and_then(|id| crate::session::web_session(ctx, &id))
        .and_then(|s| s.user)
        .is_some()
}

/// Close every connection whose session ended (logout, password change or
/// reset, account reset), so a device that ignores `force_logout` stops
/// receiving order and position pushes (security review S-09). Returns how
/// many were closed.
pub fn disconnect_signed_out(ctx: &AppState, io: &SocketIo) -> usize {
    let mut closed = 0;
    for socket in io.sockets() {
        if !signed_in(ctx, &socket) && socket.disconnect().is_ok() {
            closed += 1;
        }
    }
    if closed > 0 {
        tracing::info!("Closed {} live update connection(s) after sign-out", closed);
    }
    closed
}

/// The layer and its handle. The handlers reach the app through a weak
/// reference: the app keeps the handle (`SocketEmitter`), so a strong one
/// here was a cycle that kept every context that built the HTTP app alive
/// after its last user dropped it, with its databases and their threads (a
/// test process reached macOS's 2,048 threads that way).
pub fn layer(ctx: &Arc<AppState>) -> (SocketIoLayer, SocketIo) {
    let (layer, io) = SocketIo::builder()
        .with_state(Arc::downgrade(ctx))
        .build_layer();
    io.ns("/", on_connect);
    (layer, io)
}
