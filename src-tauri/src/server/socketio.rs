//! Socket.IO on the main port (`/socket.io`), like Flask-SocketIO on the
//! web. Only a browser with a signed-in session may stay connected; the
//! subscribers push the web's event names through `SocketEmitter`.

use crate::server::middleware::cookie_value;
use crate::session::web::COOKIE_NAME;
use crate::state::AppState;
use socketioxide::extract::{SocketRef, State};
use socketioxide::layer::SocketIoLayer;
use socketioxide::SocketIo;
use std::sync::Arc;

fn on_connect(socket: SocketRef, State(ctx): State<Arc<AppState>>) {
    let now = ctx.now();
    let signed_in = cookie_value(&socket.req_parts().headers, COOKIE_NAME)
        .and_then(|id| ctx.sessions.get(&id, now))
        .and_then(|s| s.user)
        .is_some();
    if !signed_in {
        let _ = socket.disconnect();
        return;
    }
    crate::server::routes::strategy_module::register_socket_handlers(&socket);
}

pub fn layer(ctx: Arc<AppState>) -> (SocketIoLayer, SocketIo) {
    let (layer, io) = SocketIo::builder().with_state(ctx).build_layer();
    io.ns("/", on_connect);
    (layer, io)
}
