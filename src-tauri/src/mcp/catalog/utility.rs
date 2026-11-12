//! The `utility` toolset: the OpenAlgo version, the order constants, and
//! Telegram, Slack and Discord alerts.

use crate::mcp::schema::{Dflt as D, Param, Ty::*};
use crate::mcp::{Risk, Scope, ToolDef};

pub const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "get_openalgo_version",
        title: "Get OpenAlgo Version",
        toolset: "utility",
        scope: Scope::ReadMarket,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: false,
        description: r#"Get the OpenAlgo library version."#,
        params: &[],
    },
    ToolDef {
        name: "validate_order_constants",
        title: "Validate Order Constants",
        toolset: "utility",
        scope: Scope::ReadMarket,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: false,
        description: r#"Display all valid order constants for reference."#,
        params: &[],
    },
    ToolDef {
        name: "send_telegram_alert",
        title: "Send Telegram Alert",
        toolset: "utility",
        scope: Scope::ReadAccount,
        risk: Risk::BrokerStructured,
        write: true,
        destructive: false,
        open_world: true,
        description: r#"Send a Telegram alert notification.

    Args:
        username: OpenAlgo login ID/username
        message: Alert message to send
        priority: Notification priority (1-10, default 5). Higher values may be used
                  by the bot for emphasis/sorting depending on configuration.

    Returns:
        JSON with status and message"#,
        params: &[
            Param {
                name: "username",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "message",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "priority",
                ty: Int,
                default: D::Int(5),
            },
        ],
    },
    ToolDef {
        name: "send_slack_alert",
        title: "Send Slack Alert",
        toolset: "utility",
        scope: Scope::ReadAccount,
        risk: Risk::BrokerStructured,
        write: true,
        destructive: false,
        open_world: true,
        description: r#"Post a message to the trader's Slack channel through its saved webhook.

    The webhook is set up on OpenAlgo's /slack page, and Slack must be turned
    on from the /alerts page; otherwise the call is refused with a message
    saying which step is missing. The message goes to the channel the trader
    chose, which other people may read.

    Args:
        message: Text to post, at most 4000 characters. Slack's &, < and > are
                 escaped, so no one is mentioned or pinged.
        wait_for_delivery: False (default) queues the message and returns at
                 once. True posts it first and reports whether Slack accepted it.

    Returns:
        JSON with status ('success' or 'error') and message."#,
        params: &[
            Param {
                name: "message",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "wait_for_delivery",
                ty: Bool,
                default: D::Bool(false),
            },
        ],
    },
    ToolDef {
        name: "send_discord_alert",
        title: "Send Discord Alert",
        toolset: "utility",
        scope: Scope::ReadAccount,
        risk: Risk::BrokerStructured,
        write: true,
        destructive: false,
        open_world: true,
        description: r#"Post a message to the trader's Discord channel through its saved webhook.

    The webhook is set up on OpenAlgo's /discord page, and Discord must be
    turned on from the /alerts page; otherwise the call is refused with a
    message saying which step is missing. The message goes to the channel the
    trader chose, which other people may read.

    Args:
        message: Text to post, at most 2000 characters. Mentions are never
                 resolved, so no one is pinged.
        wait_for_delivery: False (default) queues the message and returns at
                 once. True posts it first and reports whether Discord accepted it.

    Returns:
        JSON with status ('success' or 'error') and message."#,
        params: &[
            Param {
                name: "message",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "wait_for_delivery",
                ty: Bool,
                default: D::Bool(false),
            },
        ],
    },
];
