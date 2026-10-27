//! Order endpoints. Rate limits: placeorder, modifyorder, cancelorder,
//! optionsorder and optionsmultiorder share the order bucket (10/s);
//! placesmartorder has its own 10/s bucket; the rest use the API bucket.

use super::{load, send, Auth, Style};
use crate::server::middleware::ClientIp;
use crate::services::schemas;
use crate::services::{
    batch_order_service as batch, options_order_service as options, order_service as orders, Route,
};
use crate::state::AppState;
use axum::{
    extract::{Request, State},
    response::Response,
};
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

macro_rules! order_endpoint {
    ($name:ident, $schema:path, $style:expr, $svc:path) => {
        pub async fn $name(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
            match load(&ctx, ip, req, $schema(), $style, Auth::Broker).await {
                Ok(v) => send($svc(&ctx, &v, Route::API).await),
                Err(r) => *r,
            }
        }
    };
}

order_endpoint!(placeorder, schemas::order, Style::Py, orders::place_order);
order_endpoint!(
    placesmartorder,
    schemas::smart_order,
    Style::PyOrder("placesmartorder"),
    orders::place_smart_order
);
order_endpoint!(
    modifyorder,
    schemas::modify_order,
    Style::PyOrder("modifyorder"),
    orders::modify_order
);
order_endpoint!(
    cancelorder,
    schemas::cancel_order,
    Style::PyOrder("cancelorder"),
    orders::cancel_order
);
order_endpoint!(
    cancelallorder,
    schemas::strategy_only,
    Style::PyOrder("cancelallorder"),
    orders::cancel_all_orders
);
order_endpoint!(
    closeposition,
    schemas::strategy_only,
    Style::PyOrder("closeposition"),
    orders::close_position
);
order_endpoint!(
    basketorder,
    schemas::basket_order,
    Style::PyOrder("basketorder"),
    batch::basket_order
);
order_endpoint!(
    splitorder,
    schemas::split_order,
    Style::PyOrder("splitorder"),
    batch::split_order
);
order_endpoint!(
    optionsorder,
    schemas::options_order,
    Style::Envelope("Validation error"),
    options::options_order
);
order_endpoint!(
    optionsmultiorder,
    schemas::options_multi_order,
    Style::Envelope("Validation error"),
    options::options_multi_order
);
