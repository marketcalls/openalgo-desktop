//! Typed in-process events, mirroring the web's `events/` package.
//!
//! Services publish; subscribers (registered once at startup) do the side
//! effects: order and analyzer logging, Socket.IO pushes, alerts. A new side
//! effect is a new subscriber, never a line in the order path.

pub mod bus;
pub mod subscribers;

use serde::Serialize;
use serde_json::Value;

pub use bus::{BusStats, EventBus, Lane, Subscriber};

/// Topic names, byte-identical to the web.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Topic {
    OrderPlaced,
    OrderFailed,
    OrderNoAction,
    OrderModified,
    OrderModifyFailed,
    OrderCancelled,
    OrderCancelFailed,
    OrderUpdate,
    AllOrdersCancelled,
    PositionClosed,
    BasketCompleted,
    SplitCompleted,
    OptionsCompleted,
    MultiOrderCompleted,
    AnalyzerError,
    SandboxOrderFilled,
    SandboxAutoSquareoff,
    SandboxT1Settlement,
    GttPlaced,
    GttFailed,
    GttModified,
    GttModifyFailed,
    GttCancelled,
    GttCancelFailed,
    GttTriggered,
    GttExpired,
    BrokerConnected,
    BrokerSessionEnded,
    MasterContractDownload,
    CacheLoaded,
    ForceLogout,
    PendingOrderCreated,
    PendingOrderUpdated,
    /// Historify download, job and schedule progress.
    Historify,
}

impl Topic {
    pub fn as_str(&self) -> &'static str {
        match self {
            Topic::OrderPlaced => "order.placed",
            Topic::OrderFailed => "order.failed",
            Topic::OrderNoAction => "order.no_action",
            Topic::OrderModified => "order.modified",
            Topic::OrderModifyFailed => "order.modify_failed",
            Topic::OrderCancelled => "order.cancelled",
            Topic::OrderCancelFailed => "order.cancel_failed",
            Topic::OrderUpdate => "order.update",
            Topic::AllOrdersCancelled => "orders.all_cancelled",
            Topic::PositionClosed => "position.closed",
            Topic::BasketCompleted => "basket.completed",
            Topic::SplitCompleted => "split.completed",
            Topic::OptionsCompleted => "options.completed",
            Topic::MultiOrderCompleted => "multiorder.completed",
            Topic::AnalyzerError => "analyzer.error",
            Topic::SandboxOrderFilled => "sandbox.order_filled",
            Topic::SandboxAutoSquareoff => "sandbox.auto_squareoff",
            Topic::SandboxT1Settlement => "sandbox.t1_settlement",
            Topic::GttPlaced => "gtt.placed",
            Topic::GttFailed => "gtt.failed",
            Topic::GttModified => "gtt.modified",
            Topic::GttModifyFailed => "gtt.modify_failed",
            Topic::GttCancelled => "gtt.cancelled",
            Topic::GttCancelFailed => "gtt.cancel_failed",
            Topic::GttTriggered => "gtt.triggered",
            Topic::GttExpired => "gtt.expired",
            Topic::BrokerConnected => "broker.connected",
            Topic::BrokerSessionEnded => "broker.session_ended",
            Topic::MasterContractDownload => "master_contract.download",
            Topic::CacheLoaded => "cache.loaded",
            Topic::ForceLogout => "session.force_logout",
            Topic::PendingOrderCreated => "action_center.pending_order_created",
            Topic::PendingOrderUpdated => "action_center.pending_order_updated",
            Topic::Historify => "historify.update",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Live,
    Analyze,
}

impl Mode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Live => "live",
            Mode::Analyze => "analyze",
        }
    }
}

/// Fields every order event carries (web `OrderEvent` base).
#[derive(Debug, Clone)]
pub struct OrderMeta {
    pub mode: Mode,
    pub api_type: String,
    pub request_data: Value,
    pub response_data: Value,
}

#[derive(Debug, Clone, Default)]
pub struct OrderUpdate {
    pub mode: String,
    pub broker: String,
    pub orderid: String,
    pub symbol: String,
    pub exchange: String,
    pub action: String,
    pub quantity: i64,
    pub price: f64,
    pub trigger_price: f64,
    pub pricetype: String,
    pub product: String,
    pub order_status: String,
    pub filled_quantity: i64,
    pub pending_quantity: i64,
    pub average_price: f64,
    pub rejection_reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GttKind {
    Placed,
    Failed,
    Modified,
    ModifyFailed,
    Cancelled,
    CancelFailed,
    Triggered,
    Expired,
}

impl GttKind {
    pub fn name(&self) -> &'static str {
        match self {
            GttKind::Placed => "placed",
            GttKind::Failed => "failed",
            GttKind::Modified => "modified",
            GttKind::ModifyFailed => "modify_failed",
            GttKind::Cancelled => "cancelled",
            GttKind::CancelFailed => "cancel_failed",
            GttKind::Triggered => "triggered",
            GttKind::Expired => "expired",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEndReason {
    Logout,
    DailyExpiry,
    PasswordChanged,
}

#[derive(Debug, Clone)]
pub enum Event {
    OrderPlaced {
        meta: OrderMeta,
        strategy: String,
        symbol: String,
        exchange: String,
        action: String,
        quantity: i64,
        pricetype: String,
        product: String,
        orderid: String,
    },
    OrderFailed {
        meta: OrderMeta,
        symbol: String,
        exchange: String,
        error_message: String,
    },
    OrderNoAction {
        meta: OrderMeta,
        symbol: String,
        exchange: String,
        message: String,
    },
    OrderModified {
        meta: OrderMeta,
        symbol: String,
        exchange: String,
        orderid: String,
    },
    OrderModifyFailed {
        meta: OrderMeta,
        symbol: String,
        orderid: String,
        error_message: String,
    },
    OrderCancelled {
        meta: OrderMeta,
        orderid: String,
        status: String,
    },
    OrderCancelFailed {
        meta: OrderMeta,
        orderid: String,
        error_message: String,
    },
    OrderUpdate(OrderUpdate),
    AllOrdersCancelled {
        meta: OrderMeta,
        canceled_count: i64,
        failed_count: i64,
    },
    PositionClosed {
        meta: OrderMeta,
        message: Option<String>,
    },
    BasketCompleted {
        meta: OrderMeta,
        strategy: Option<String>,
        successful: i64,
        total: i64,
    },
    SplitCompleted {
        meta: OrderMeta,
        symbol: Option<String>,
        action: Option<String>,
        exchange: Option<String>,
        pricetype: Option<String>,
        product: Option<String>,
        successful: i64,
        total: i64,
    },
    OptionsCompleted {
        meta: OrderMeta,
        symbol: String,
        action: String,
        exchange: String,
        pricetype: Option<String>,
        product: Option<String>,
        successful: i64,
        total: i64,
    },
    MultiOrderCompleted {
        meta: OrderMeta,
        underlying: String,
        strategy: Option<String>,
        exchange: String,
        successful_legs: i64,
        failed_legs: i64,
        total: i64,
    },
    AnalyzerError {
        meta: OrderMeta,
    },
    SandboxOrderFilled {
        meta: OrderMeta,
    },
    SandboxAutoSquareoff {
        meta: OrderMeta,
    },
    SandboxT1Settlement {
        meta: OrderMeta,
    },
    Gtt {
        kind: GttKind,
        meta: OrderMeta,
        symbol: String,
        exchange: String,
        trigger_id: String,
        triggered_order_id: String,
    },
    BrokerConnected {
        broker: String,
        user_id: Option<String>,
    },
    BrokerSessionEnded {
        reason: SessionEndReason,
    },
    MasterContractDownload {
        broker: String,
        status: String,
        message: String,
    },
    CacheLoaded {
        payload: Value,
    },
    ForceLogout {
        message: String,
    },
    /// An order queued for approval in the Action Center (Semi-Auto mode);
    /// the payload is the web's `pending_order_created` Socket.IO body.
    PendingOrderCreated {
        payload: Value,
    },
    /// An Action Center order approved, rejected, deleted or returned; the
    /// payload is the web's `pending_order_updated` Socket.IO body.
    PendingOrderUpdated {
        payload: Value,
    },
    /// A Historify push: the web's Socket.IO event name and its payload
    /// (`historify_progress`, `historify_job_complete`, ...).
    Historify {
        event: &'static str,
        payload: Value,
    },
}

impl Event {
    pub fn topic(&self) -> Topic {
        match self {
            Event::OrderPlaced { .. } => Topic::OrderPlaced,
            Event::OrderFailed { .. } => Topic::OrderFailed,
            Event::OrderNoAction { .. } => Topic::OrderNoAction,
            Event::OrderModified { .. } => Topic::OrderModified,
            Event::OrderModifyFailed { .. } => Topic::OrderModifyFailed,
            Event::OrderCancelled { .. } => Topic::OrderCancelled,
            Event::OrderCancelFailed { .. } => Topic::OrderCancelFailed,
            Event::OrderUpdate(_) => Topic::OrderUpdate,
            Event::AllOrdersCancelled { .. } => Topic::AllOrdersCancelled,
            Event::PositionClosed { .. } => Topic::PositionClosed,
            Event::BasketCompleted { .. } => Topic::BasketCompleted,
            Event::SplitCompleted { .. } => Topic::SplitCompleted,
            Event::OptionsCompleted { .. } => Topic::OptionsCompleted,
            Event::MultiOrderCompleted { .. } => Topic::MultiOrderCompleted,
            Event::AnalyzerError { .. } => Topic::AnalyzerError,
            Event::SandboxOrderFilled { .. } => Topic::SandboxOrderFilled,
            Event::SandboxAutoSquareoff { .. } => Topic::SandboxAutoSquareoff,
            Event::SandboxT1Settlement { .. } => Topic::SandboxT1Settlement,
            Event::Gtt { kind, .. } => match kind {
                GttKind::Placed => Topic::GttPlaced,
                GttKind::Failed => Topic::GttFailed,
                GttKind::Modified => Topic::GttModified,
                GttKind::ModifyFailed => Topic::GttModifyFailed,
                GttKind::Cancelled => Topic::GttCancelled,
                GttKind::CancelFailed => Topic::GttCancelFailed,
                GttKind::Triggered => Topic::GttTriggered,
                GttKind::Expired => Topic::GttExpired,
            },
            Event::BrokerConnected { .. } => Topic::BrokerConnected,
            Event::BrokerSessionEnded { .. } => Topic::BrokerSessionEnded,
            Event::MasterContractDownload { .. } => Topic::MasterContractDownload,
            Event::CacheLoaded { .. } => Topic::CacheLoaded,
            Event::ForceLogout { .. } => Topic::ForceLogout,
            Event::PendingOrderCreated { .. } => Topic::PendingOrderCreated,
            Event::PendingOrderUpdated { .. } => Topic::PendingOrderUpdated,
            Event::Historify { .. } => Topic::Historify,
        }
    }

    /// The order-event base fields, for events that carry them.
    pub fn meta(&self) -> Option<&OrderMeta> {
        match self {
            Event::OrderPlaced { meta, .. }
            | Event::OrderFailed { meta, .. }
            | Event::OrderNoAction { meta, .. }
            | Event::OrderModified { meta, .. }
            | Event::OrderModifyFailed { meta, .. }
            | Event::OrderCancelled { meta, .. }
            | Event::OrderCancelFailed { meta, .. }
            | Event::AllOrdersCancelled { meta, .. }
            | Event::PositionClosed { meta, .. }
            | Event::BasketCompleted { meta, .. }
            | Event::SplitCompleted { meta, .. }
            | Event::OptionsCompleted { meta, .. }
            | Event::MultiOrderCompleted { meta, .. }
            | Event::AnalyzerError { meta }
            | Event::SandboxOrderFilled { meta }
            | Event::SandboxAutoSquareoff { meta }
            | Event::SandboxT1Settlement { meta }
            | Event::Gtt { meta, .. } => Some(meta),
            _ => None,
        }
    }
}
