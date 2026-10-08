//! The tool catalogue: name, title, toolset, scope, output risk, hints,
//! description and parameters of every tool in the web's MCP server
//! (`openalgo/mcp/mcpserver.py`, scopes from `utils/mcp_tool_registry.py`).
//!
//! Ported verbatim from the web's `tools/list`; the contract test
//! (`tests/it/mcp_contract.rs`) compares the descriptors built from this
//! table with `tests/fixtures/mcp/tools_list.json` and fails on any drift.
//! Edit a description here only together with that fixture.

use super::schema::{Dflt as D, Param, Ty::*};
use super::{Risk, Scope, ToolDef};

pub const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "place_order",
        title: "Place Order",
        toolset: "orders",
        scope: Scope::WriteOrders,
        risk: Risk::BrokerStructured,
        write: true,
        destructive: true,
        open_world: true,
        description: r#"Place a new order (market or limit).

    Args:
        symbol: Stock symbol (e.g., 'RELIANCE')
        quantity: Number of shares
        action: 'BUY' or 'SELL'
        exchange: 'NSE', 'NFO', 'CDS', 'BSE', 'BFO', 'BCD', 'MCX', 'NCDEX'
        price_type: 'MARKET', 'LIMIT', 'SL', 'SL-M'
        product: 'CNC', 'NRML', 'MIS'
        strategy: Strategy name (defaults to 'python mcp')
        price: Limit price (required for LIMIT orders)
        trigger_price: Trigger price (required for SL and SL-M orders)
        disclosed_quantity: Disclosed quantity"#,
        params: &[
            Param {
                name: "symbol",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "quantity",
                ty: Int,
                default: D::Required,
            },
            Param {
                name: "action",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "exchange",
                ty: Str,
                default: D::Str("NSE"),
            },
            Param {
                name: "price_type",
                ty: Str,
                default: D::Str("MARKET"),
            },
            Param {
                name: "product",
                ty: Str,
                default: D::Str("MIS"),
            },
            Param {
                name: "strategy",
                ty: Str,
                default: D::Str("python mcp"),
            },
            Param {
                name: "price",
                ty: OptNum,
                default: D::Null,
            },
            Param {
                name: "trigger_price",
                ty: OptNum,
                default: D::Null,
            },
            Param {
                name: "disclosed_quantity",
                ty: OptInt,
                default: D::Null,
            },
        ],
    },
    ToolDef {
        name: "place_smart_order",
        title: "Place Smart Order",
        toolset: "orders",
        scope: Scope::WriteOrders,
        risk: Risk::BrokerStructured,
        write: true,
        destructive: true,
        open_world: true,
        description: r#"Place a smart order that considers the current position size (auto-calculates delta
    between requested and current size before sending to the broker).

    Args:
        symbol: Stock symbol
        quantity: Target quantity
        action: 'BUY' or 'SELL'
        position_size: Current position size
        exchange: Exchange name
        price_type: 'MARKET', 'LIMIT', 'SL', 'SL-M'
        product: 'CNC', 'NRML', 'MIS'
        strategy: Strategy name (defaults to 'python mcp')
        price: Limit price (required for LIMIT orders)
        trigger_price: Trigger price (required for SL / SL-M orders)
        disclosed_quantity: Disclosed quantity"#,
        params: &[
            Param {
                name: "symbol",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "quantity",
                ty: Int,
                default: D::Required,
            },
            Param {
                name: "action",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "position_size",
                ty: Int,
                default: D::Required,
            },
            Param {
                name: "exchange",
                ty: Str,
                default: D::Str("NSE"),
            },
            Param {
                name: "price_type",
                ty: Str,
                default: D::Str("MARKET"),
            },
            Param {
                name: "product",
                ty: Str,
                default: D::Str("MIS"),
            },
            Param {
                name: "strategy",
                ty: Str,
                default: D::Str("python mcp"),
            },
            Param {
                name: "price",
                ty: OptNum,
                default: D::Null,
            },
            Param {
                name: "trigger_price",
                ty: OptNum,
                default: D::Null,
            },
            Param {
                name: "disclosed_quantity",
                ty: OptInt,
                default: D::Null,
            },
        ],
    },
    ToolDef {
        name: "place_basket_order",
        title: "Place Basket Order",
        toolset: "orders",
        scope: Scope::WriteOrders,
        risk: Risk::BrokerStructured,
        write: true,
        destructive: true,
        open_world: true,
        description: r#"Place multiple orders in a basket.

    Args:
        orders: List of order dictionaries. Each order should contain:
            - symbol (str): Trading symbol. Required.
            - exchange (str): Exchange code. Required.
            - action (str): BUY or SELL. Required.
            - quantity (int/str): Quantity to trade. Required.
            - pricetype (str): MARKET, LIMIT, SL, SL-M. Optional, defaults to MARKET.
            - product (str): MIS, CNC, NRML. Optional, defaults to MIS.
            - price (str): Required for LIMIT orders.
            - trigger_price (str): Required for SL orders.
        strategy: Strategy name (default: Python)

        Example: [
            {"symbol": "BHEL", "exchange": "NSE", "action": "BUY", "quantity": 1, "pricetype": "MARKET", "product": "MIS"},
            {"symbol": "ZOMATO", "exchange": "NSE", "action": "SELL", "quantity": 1, "pricetype": "MARKET", "product": "MIS"}
        ]

    Returns:
        JSON with results for each order including orderid, status, and symbol"#,
        params: &[
            Param {
                name: "orders",
                ty: ObjectList,
                default: D::Required,
            },
            Param {
                name: "strategy",
                ty: Str,
                default: D::Str("python mcp"),
            },
        ],
    },
    ToolDef {
        name: "place_split_order",
        title: "Place Split Order",
        toolset: "orders",
        scope: Scope::WriteOrders,
        risk: Risk::BrokerStructured,
        write: true,
        destructive: true,
        open_world: true,
        description: r#"Place a large order split into smaller chunks.

    Args:
        symbol: Stock symbol (e.g., 'YESBANK')
        quantity: Total quantity to trade
        split_size: Size of each split order
        action: 'BUY' or 'SELL'
        exchange: Exchange name (default: NSE)
        price_type: 'MARKET', 'LIMIT', 'SL', 'SL-M' (default: MARKET)
        product: 'MIS', 'CNC', 'NRML' (default: MIS)
        strategy: Strategy name (default: Python)
        price: Limit price (required for LIMIT orders)
        trigger_price: Trigger price (required for SL orders)
        disclosed_quantity: Disclosed quantity (optional)

    Returns:
        JSON with results array containing each split order's orderid, quantity, and status

    Example:
        # Split 105 shares into orders of 20 each (5 orders of 20 + 1 order of 5)
        place_split_order("YESBANK", 105, 20, "SELL", "NSE")"#,
        params: &[
            Param {
                name: "symbol",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "quantity",
                ty: Int,
                default: D::Required,
            },
            Param {
                name: "split_size",
                ty: Int,
                default: D::Required,
            },
            Param {
                name: "action",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "exchange",
                ty: Str,
                default: D::Str("NSE"),
            },
            Param {
                name: "price_type",
                ty: Str,
                default: D::Str("MARKET"),
            },
            Param {
                name: "product",
                ty: Str,
                default: D::Str("MIS"),
            },
            Param {
                name: "strategy",
                ty: Str,
                default: D::Str("python mcp"),
            },
            Param {
                name: "price",
                ty: OptNum,
                default: D::Null,
            },
            Param {
                name: "trigger_price",
                ty: OptNum,
                default: D::Null,
            },
            Param {
                name: "disclosed_quantity",
                ty: OptInt,
                default: D::Null,
            },
        ],
    },
    ToolDef {
        name: "place_options_order",
        title: "Place Options Order",
        toolset: "orders",
        scope: Scope::WriteOrders,
        risk: Risk::BrokerStructured,
        write: true,
        destructive: true,
        open_world: true,
        description: r#"Place an options order with ATM/ITM/OTM offset.

    Args:
        underlying: Underlying symbol (e.g., 'NIFTY', 'BANKNIFTY', 'NIFTY28OCT25FUT')
        exchange: Exchange for underlying ('NSE_INDEX', 'BSE_INDEX', 'NFO')
        offset: Strike offset - 'ATM', 'ITM1'-'ITM50', 'OTM1'-'OTM50'
        option_type: 'CE' for Call or 'PE' for Put
        action: 'BUY' or 'SELL'
        quantity: Absolute quantity — must be a multiple of the contract lot size.
                  Do NOT hardcode lot size — call get_option_symbol() or get_option_chain()
                  first to read the current 'lotsize' from the broker master contract,
                  then pass quantity = lots * lotsize.
        expiry_date: Expiry date in format 'DDMMMYY' (e.g., '28OCT25'). Optional if underlying includes expiry.
        strategy: Strategy name (default: Python)
        price_type: 'MARKET', 'LIMIT', 'SL', 'SL-M' (default: MARKET)
        product: 'MIS', 'NRML' (default: MIS). Note: CNC not supported for options.
        price: Limit price (required for LIMIT orders)
        trigger_price: Trigger price (required for SL and SL-M orders)
        disclosed_quantity: Disclosed quantity (optional)

    Returns:
        JSON with orderid, symbol, underlying_ltp, offset, option_type, mode

    Example:
        # Basic ATM call order
        place_options_order("NIFTY", "NSE_INDEX", "ATM", "CE", "BUY", 75, "28NOV25")

        # Using future as underlying (expiry auto-detected)
        place_options_order("NIFTY28OCT25FUT", "NFO", "ITM2", "CE", "BUY", 75)"#,
        params: &[
            Param {
                name: "underlying",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "exchange",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "offset",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "option_type",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "action",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "quantity",
                ty: Int,
                default: D::Required,
            },
            Param {
                name: "expiry_date",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "strategy",
                ty: Str,
                default: D::Str("python mcp"),
            },
            Param {
                name: "price_type",
                ty: Str,
                default: D::Str("MARKET"),
            },
            Param {
                name: "product",
                ty: Str,
                default: D::Str("MIS"),
            },
            Param {
                name: "price",
                ty: OptNum,
                default: D::Null,
            },
            Param {
                name: "trigger_price",
                ty: OptNum,
                default: D::Null,
            },
            Param {
                name: "disclosed_quantity",
                ty: OptInt,
                default: D::Null,
            },
        ],
    },
    ToolDef {
        name: "place_options_multi_order",
        title: "Place Multi-Leg Options Order",
        toolset: "orders",
        scope: Scope::WriteOrders,
        risk: Risk::BrokerStructured,
        write: true,
        destructive: true,
        open_world: true,
        description: r#"Place a multi-leg options order (spreads, iron condor, straddles, etc.).
    BUY legs are executed first for margin efficiency, then SELL legs.

    Args:
        strategy: Strategy name (defaults to 'python mcp'). Give each multi-leg trade
                  a meaningful name (e.g., 'nifty iron condor') to make tracking easier.
        underlying: Underlying symbol (e.g., 'NIFTY', 'BANKNIFTY', 'NIFTY28OCT25FUT')
        exchange: Exchange for underlying ('NSE_INDEX', 'BSE_INDEX', 'NFO')
        legs: List of leg dictionaries (1-20 legs). Each leg must contain:
            Required:
            - offset: Strike offset ('ATM', 'ITM1'-'ITM50', 'OTM1'-'OTM50')
            - option_type: 'CE' for Call or 'PE' for Put
            - action: 'BUY' or 'SELL'
            - quantity: Absolute quantity — must be a multiple of the contract lot size.
                        Do NOT hardcode lot size. Look up the current 'lotsize' per leg
                        using get_option_symbol() or get_option_chain() first, then pass
                        quantity = lots * lotsize. Lot sizes can change (e.g., NIFTY has
                        changed multiple times) and differ by underlying.
            Optional:
            - expiry_date: Per-leg expiry in DDMMMYY format for diagonal/calendar spreads
            - pricetype: 'MARKET', 'LIMIT', 'SL', 'SL-M' (default: MARKET)
            - product: 'MIS', 'NRML' (default: MIS)
            - price: Limit price for LIMIT orders
            - trigger_price: Trigger price for SL orders
            - disclosed_quantity: Disclosed quantity
        expiry_date: Default expiry date in format 'DDMMMYY' (e.g., '25NOV25') for all legs

    Returns:
        JSON with underlying, underlying_ltp, mode, and results array containing each leg's
        orderid, symbol, offset, option_type, action, and status

    Example - Iron Condor (same expiry):
        [
            {"offset": "OTM10", "option_type": "CE", "action": "BUY", "quantity": 75},
            {"offset": "OTM10", "option_type": "PE", "action": "BUY", "quantity": 75},
            {"offset": "OTM5", "option_type": "CE", "action": "SELL", "quantity": 75},
            {"offset": "OTM5", "option_type": "PE", "action": "SELL", "quantity": 75}
        ]

    Example - Bull Call Spread with NRML:
        [
            {"offset": "ATM", "option_type": "CE", "action": "BUY", "quantity": 75, "product": "NRML"},
            {"offset": "OTM1", "option_type": "CE", "action": "SELL", "quantity": 75, "product": "NRML"}
        ]

    Example - Diagonal Spread (different expiry):
        [
            {"offset": "ITM2", "option_type": "CE", "action": "BUY", "quantity": 75, "expiry_date": "30DEC25"},
            {"offset": "OTM2", "option_type": "CE", "action": "SELL", "quantity": 75, "expiry_date": "25NOV25"}
        ]

    Example - Long Straddle with LIMIT orders:
        [
            {"offset": "ATM", "option_type": "CE", "action": "BUY", "quantity": 30, "pricetype": "LIMIT", "price": 250.0},
            {"offset": "ATM", "option_type": "PE", "action": "BUY", "quantity": 30, "pricetype": "LIMIT", "price": 250.0}
        ]"#,
        params: &[
            Param {
                name: "underlying",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "exchange",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "legs",
                ty: ObjectList,
                default: D::Required,
            },
            Param {
                name: "expiry_date",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "strategy",
                ty: Str,
                default: D::Str("python mcp"),
            },
        ],
    },
    ToolDef {
        name: "modify_order",
        title: "Modify Order",
        toolset: "orders",
        scope: Scope::WriteOrders,
        risk: Risk::BrokerStructured,
        write: true,
        destructive: true,
        open_world: true,
        description: r#"Modify an existing order.

    Args:
        order_id: Order ID to modify
        symbol: Stock symbol
        action: 'BUY' or 'SELL'
        exchange: Exchange name
        product: 'CNC', 'NRML', 'MIS'
        quantity: New quantity
        price: New price (required by the API — use current price if unchanged)
        strategy: Strategy name (defaults to 'python mcp')
        price_type: 'MARKET', 'LIMIT', 'SL', 'SL-M' (defaults to 'LIMIT')
        trigger_price: New trigger price for SL/SL-M orders (default 0)
        disclosed_quantity: New disclosed quantity (default 0)"#,
        params: &[
            Param {
                name: "order_id",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "symbol",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "action",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "exchange",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "product",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "quantity",
                ty: Int,
                default: D::Required,
            },
            Param {
                name: "price",
                ty: Num,
                default: D::Required,
            },
            Param {
                name: "strategy",
                ty: Str,
                default: D::Str("python mcp"),
            },
            Param {
                name: "price_type",
                ty: Str,
                default: D::Str("LIMIT"),
            },
            Param {
                name: "trigger_price",
                ty: Num,
                default: D::Int(0),
            },
            Param {
                name: "disclosed_quantity",
                ty: Int,
                default: D::Int(0),
            },
        ],
    },
    ToolDef {
        name: "cancel_order",
        title: "Cancel Order",
        toolset: "orders",
        scope: Scope::WriteOrders,
        risk: Risk::BrokerStructured,
        write: true,
        destructive: true,
        open_world: true,
        description: r#"Cancel a specific order.

    Args:
        order_id: Order ID to cancel
        strategy: Strategy name (defaults to 'python mcp')"#,
        params: &[
            Param {
                name: "order_id",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "strategy",
                ty: Str,
                default: D::Str("python mcp"),
            },
        ],
    },
    ToolDef {
        name: "cancel_all_orders",
        title: "Cancel All Orders",
        toolset: "orders",
        scope: Scope::WriteOrders,
        risk: Risk::BrokerStructured,
        write: true,
        destructive: true,
        open_world: true,
        description: r#"Cancel all open orders for a strategy.

    Args:
        strategy: Strategy name (defaults to 'python mcp')"#,
        params: &[Param {
            name: "strategy",
            ty: Str,
            default: D::Str("python mcp"),
        }],
    },
    ToolDef {
        name: "close_all_positions",
        title: "Close All Positions",
        toolset: "orders",
        scope: Scope::WriteOrders,
        risk: Risk::BrokerStructured,
        write: true,
        destructive: true,
        open_world: true,
        description: r#"Close all open positions for a strategy.

    Args:
        strategy: Strategy name (defaults to 'python mcp')"#,
        params: &[Param {
            name: "strategy",
            ty: Str,
            default: D::Str("python mcp"),
        }],
    },
    ToolDef {
        name: "get_open_position",
        title: "Get Open Position",
        toolset: "account",
        scope: Scope::ReadAccount,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Get current open position for a specific instrument.

    Args:
        symbol: Stock symbol
        exchange: Exchange name
        product: Product type ('CNC', 'NRML', 'MIS')
        strategy: Strategy name (defaults to 'python mcp')"#,
        params: &[
            Param {
                name: "symbol",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "exchange",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "product",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "strategy",
                ty: Str,
                default: D::Str("python mcp"),
            },
        ],
    },
    ToolDef {
        name: "get_order_status",
        title: "Get Order Status",
        toolset: "account",
        scope: Scope::ReadAccount,
        risk: Risk::ExternalText,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Get status of a specific order.

    Args:
        order_id: Order ID
        strategy: Strategy name (defaults to 'python mcp')"#,
        params: &[
            Param {
                name: "order_id",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "strategy",
                ty: Str,
                default: D::Str("python mcp"),
            },
        ],
    },
    ToolDef {
        name: "get_order_book",
        title: "Get Order Book",
        toolset: "account",
        scope: Scope::ReadAccount,
        risk: Risk::ExternalText,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Get all orders from the order book."#,
        params: &[],
    },
    ToolDef {
        name: "get_trade_book",
        title: "Get Trade Book",
        toolset: "account",
        scope: Scope::ReadAccount,
        risk: Risk::ExternalText,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Get all executed trades."#,
        params: &[],
    },
    ToolDef {
        name: "get_position_book",
        title: "Get Position Book",
        toolset: "account",
        scope: Scope::ReadAccount,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Get all current positions."#,
        params: &[],
    },
    ToolDef {
        name: "get_holdings",
        title: "Get Holdings",
        toolset: "account",
        scope: Scope::ReadAccount,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Get all holdings (long-term investments)."#,
        params: &[],
    },
    ToolDef {
        name: "get_funds",
        title: "Get Funds",
        toolset: "account",
        scope: Scope::ReadAccount,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Get account funds and margin information."#,
        params: &[],
    },
    ToolDef {
        name: "calculate_margin",
        title: "Calculate Margin",
        toolset: "account",
        scope: Scope::ReadAccount,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Calculate margin requirements for positions.

    Args:
        positions: List of position dictionaries
        Example: [{"symbol": "NIFTY25NOV2525000CE", "exchange": "NFO", "action": "BUY", "product": "NRML", "pricetype": "MARKET", "quantity": "75"}]

        For Futures: [{"symbol": "NIFTY25NOV25FUT", "exchange": "NFO", "action": "BUY", "product": "NRML", "pricetype": "MARKET", "quantity": "25"}]
        For Options: [{"symbol": "NIFTY25NOV2525500CE", "exchange": "NFO", "action": "BUY", "product": "NRML", "pricetype": "MARKET", "quantity": "75"}]

    Returns:
        JSON with total_margin_required, span_margin, and exposure_margin"#,
        params: &[Param {
            name: "positions",
            ty: ObjectList,
            default: D::Required,
        }],
    },
    ToolDef {
        name: "get_quote",
        title: "Get Quote",
        toolset: "marketdata",
        scope: Scope::ReadMarket,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Get current quote for a symbol.

    Args:
        symbol: Stock symbol
        exchange: Exchange name"#,
        params: &[
            Param {
                name: "symbol",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "exchange",
                ty: Str,
                default: D::Str("NSE"),
            },
        ],
    },
    ToolDef {
        name: "get_multi_quotes",
        title: "Get Multiple Quotes",
        toolset: "marketdata",
        scope: Scope::ReadMarket,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Get real-time quotes for multiple symbols in a single request.

    Args:
        symbols: List of symbol-exchange pairs
        Example: [{"symbol": "RELIANCE", "exchange": "NSE"}, {"symbol": "INFY", "exchange": "NSE"}]

    Returns:
        JSON with quotes for all requested symbols including ltp, bid, ask, open, high, low, volume, oi"#,
        params: &[Param {
            name: "symbols",
            ty: StrMapList,
            default: D::Required,
        }],
    },
    ToolDef {
        name: "get_option_chain",
        title: "Get Option Chain",
        toolset: "marketdata",
        scope: Scope::ReadMarket,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Get option chain data with real-time quotes for all strikes.

    Args:
        underlying: Underlying symbol (e.g., 'NIFTY', 'BANKNIFTY', 'RELIANCE',
                    or a future like 'NIFTY30DEC25FUT')
        exchange: Exchange for underlying ('NSE_INDEX', 'BSE_INDEX', 'NSE', 'BSE', 'NFO', 'BFO')
        expiry_date: Expiry date in DDMMMYY format (e.g., '30DEC25'). Optional when the
                     underlying already includes an expiry (e.g., 'NIFTY30DEC25FUT').
        strike_count: Number of strikes above and below ATM (1-100). If not provided, returns entire chain.

    Returns:
        JSON with:
        - underlying: Base symbol
        - underlying_ltp: Current price of underlying
        - expiry_date: Expiry date
        - atm_strike: At-The-Money strike price
        - chain: Array of strikes with CE and PE data including:
            - symbol, label (ATM/ITM1/OTM1 etc.), ltp, bid, ask, open, high, low, volume, oi, lotsize

    Note: CE and PE have different labels at the same strike:
        - Strikes below ATM: CE is ITM, PE is OTM
        - Strikes above ATM: CE is OTM, PE is ITM

    Example for 10 strikes around ATM:
        get_option_chain("NIFTY", "NSE_INDEX", "30DEC25", 10)

    Example for full chain:
        get_option_chain("NIFTY", "NSE_INDEX", "30DEC25")"#,
        params: &[
            Param {
                name: "underlying",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "exchange",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "expiry_date",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "strike_count",
                ty: OptInt,
                default: D::Null,
            },
        ],
    },
    ToolDef {
        name: "get_market_depth",
        title: "Get Market Depth",
        toolset: "marketdata",
        scope: Scope::ReadMarket,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Get market depth (order book) for a symbol.

    Args:
        symbol: Stock symbol
        exchange: Exchange name"#,
        params: &[
            Param {
                name: "symbol",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "exchange",
                ty: Str,
                default: D::Str("NSE"),
            },
        ],
    },
    ToolDef {
        name: "get_historical_data",
        title: "Get Historical Data",
        toolset: "marketdata",
        scope: Scope::ReadMarket,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Get historical OHLCV data for a symbol.

    Args:
        symbol: Stock symbol
        exchange: Exchange name
        interval: Time interval. With source='api': '1m', '3m', '5m', '10m', '15m', '30m', '1h', 'D'.
                  With source='db': also supports custom intervals (2m, 4m, 6m, 7m, 2h, 3h, 4h) and
                  daily-based (W, M, Q, Y plus multiples like 2W, 3M).
        start_date: Start date (YYYY-MM-DD). Optional — when omitted, the last `bars`
                    (default 20) most-recent bars are returned (or `lookback_days` if given).
        end_date: End date (YYYY-MM-DD). Optional — defaults to today.
        source: 'api' (default) fetches from broker API. 'db' fetches from the local
                OpenAlgo Historify DuckDB store (1m/D stored, other intervals computed via SQL).
        bars: Number of most-recent bars to return (default 20). The window is fetched
              server-side; only the last `bars` rows are sent back to keep the payload small.
              Increase only if you explicitly need more rows.
        lookback_days: When dates are omitted, fetch the last N calendar days instead of a
                       bar-count window (e.g., 30 for "last 30 days").

    Returns:
        JSON with total count, returned count, a truncated flag, and data (list of
        {timestamp, open, high, low, close, volume}) — the last `bars` rows."#,
        params: &[
            Param {
                name: "symbol",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "exchange",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "interval",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "start_date",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "end_date",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "source",
                ty: Str,
                default: D::Str("api"),
            },
            Param {
                name: "bars",
                ty: Int,
                default: D::Int(20),
            },
            Param {
                name: "lookback_days",
                ty: OptInt,
                default: D::Null,
            },
        ],
    },
    ToolDef {
        name: "search_instruments",
        title: "Search Instruments",
        toolset: "marketdata",
        scope: Scope::ReadMarket,
        risk: Risk::ExternalText,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Search for instruments by name or symbol.

    Args:
        query: Search query (e.g., 'NIFTY 26000 DEC CE', 'RELIANCE')
        exchange: Exchange to restrict the search to (NSE, BSE, NFO, BFO, MCX, NSE_INDEX, etc.).
                  Optional — when omitted, searches across all exchanges.
        instrument_type: Optional convenience filter — pass 'INDEX' to auto-rewrite
                         exchange=NSE → NSE_INDEX and BSE → BSE_INDEX."#,
        params: &[
            Param {
                name: "query",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "exchange",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "instrument_type",
                ty: OptStr,
                default: D::Null,
            },
        ],
    },
    ToolDef {
        name: "get_symbol_info",
        title: "Get Symbol Info",
        toolset: "marketdata",
        scope: Scope::ReadMarket,
        risk: Risk::ExternalText,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Get detailed information about a symbol.

    Args:
        symbol: Stock symbol
        exchange: Exchange name
        instrument_type: Optional - 'INDEX' for index symbols"#,
        params: &[
            Param {
                name: "symbol",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "exchange",
                ty: Str,
                default: D::Str("NSE"),
            },
            Param {
                name: "instrument_type",
                ty: Str,
                default: D::Null,
            },
        ],
    },
    ToolDef {
        name: "get_index_symbols",
        title: "Get Index Symbols",
        toolset: "marketdata",
        scope: Scope::ReadMarket,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: false,
        description: r#"Get the OpenAlgo-standardized index symbols for NSE or BSE.

    These are the common index names rolled out across all supported brokers via the
    OpenAlgo symbol standardization. Use exchange code 'NSE_INDEX' / 'BSE_INDEX' when
    placing orders or fetching quotes for these symbols.

    Args:
        exchange: NSE or BSE

    Returns:
        JSON with exchange, exchange_code, and the full list of standardized index
        symbols (57+ NSE, 40+ BSE)."#,
        params: &[Param {
            name: "exchange",
            ty: Str,
            default: D::Str("NSE"),
        }],
    },
    ToolDef {
        name: "get_expiry_dates",
        title: "Get Expiry Dates",
        toolset: "marketdata",
        scope: Scope::ReadMarket,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Get expiry dates for derivatives.

    Args:
        symbol: Underlying symbol
        exchange: Exchange name (typically NFO for F&O)
        instrument_type: 'options' or 'futures'"#,
        params: &[
            Param {
                name: "symbol",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "exchange",
                ty: Str,
                default: D::Str("NFO"),
            },
            Param {
                name: "instrument_type",
                ty: Str,
                default: D::Str("options"),
            },
        ],
    },
    ToolDef {
        name: "get_available_intervals",
        title: "Get Available Intervals",
        toolset: "marketdata",
        scope: Scope::ReadMarket,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Get all available time intervals for historical data."#,
        params: &[],
    },
    ToolDef {
        name: "get_option_symbol",
        title: "Get Option Symbol",
        toolset: "marketdata",
        scope: Scope::ReadMarket,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Get option symbol for specific strike and expiry.

    Args:
        underlying: Underlying symbol (e.g., 'NIFTY', 'BANKNIFTY', 'NIFTY28OCT25FUT')
        exchange: Exchange for underlying ('NSE_INDEX', 'BSE_INDEX', 'NFO', 'BFO')
        offset: Strike offset - 'ATM', 'ITM1'-'ITM50', 'OTM1'-'OTM50'
        option_type: 'CE' for Call or 'PE' for Put
        expiry_date: Expiry date in 'DDMMMYY' format (e.g., '28OCT25'). Optional when
                     the underlying already includes an expiry.

    Returns:
        JSON with symbol, exchange, lotsize, tick_size, underlying_ltp"#,
        params: &[
            Param {
                name: "underlying",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "exchange",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "offset",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "option_type",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "expiry_date",
                ty: OptStr,
                default: D::Null,
            },
        ],
    },
    ToolDef {
        name: "get_synthetic_future",
        title: "Get Synthetic Future Price",
        toolset: "marketdata",
        scope: Scope::ReadMarket,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Calculate synthetic future price using put-call parity.

    Args:
        underlying: Underlying symbol (e.g., 'NIFTY', 'BANKNIFTY')
        exchange: Exchange for underlying ('NSE_INDEX', 'BSE_INDEX')
        expiry_date: Expiry date in format 'DDMMMYY' (e.g., '25NOV25')

    Returns:
        JSON with atm_strike, expiry, status, synthetic_future_price, underlying, underlying_ltp"#,
        params: &[
            Param {
                name: "underlying",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "exchange",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "expiry_date",
                ty: Str,
                default: D::Required,
            },
        ],
    },
    ToolDef {
        name: "get_option_greeks",
        title: "Get Option Greeks",
        toolset: "marketdata",
        scope: Scope::ReadMarket,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Calculate option Greeks (Delta, Gamma, Theta, Vega, Rho) and Implied Volatility using Black-76.

    Args:
        symbol: Option symbol (e.g., 'NIFTY25NOV2526000CE'). Required.
        exchange: Exchange code ('NFO', 'BFO', 'CDS', 'MCX'). Required.
        interest_rate: Risk-free interest rate in annualized % (e.g., 6.5 for RBI repo).
                       Optional — defaults to 0.
        forward_price: Custom forward / synthetic futures price. If provided, skips the
                       underlying price fetch. Useful for illiquid underlyings (FINNIFTY,
                       MIDCPNIFTY) or custom scenario analysis.
        underlying_symbol: Custom underlying symbol (e.g., 'NIFTY', 'NIFTY30DEC25FUT').
                           Optional — auto-detected from the option symbol when omitted.
        underlying_exchange: Custom underlying exchange ('NSE_INDEX', 'NFO', etc.).
                             Optional — auto-detected when omitted.
        expiry_time: Custom expiry time in HH:MM format (e.g., '19:00'). Required for
                     MCX contracts with non-standard expiry times. Exchange defaults:
                     NFO/BFO=15:30, CDS=12:30, MCX=23:30.

    Returns:
        JSON with greeks, implied_volatility, spot_price, strike, days_to_expiry."#,
        params: &[
            Param {
                name: "symbol",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "exchange",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "interest_rate",
                ty: OptNum,
                default: D::Null,
            },
            Param {
                name: "forward_price",
                ty: OptNum,
                default: D::Null,
            },
            Param {
                name: "underlying_symbol",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "underlying_exchange",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "expiry_time",
                ty: OptStr,
                default: D::Null,
            },
        ],
    },
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
        name: "get_holidays",
        title: "Get Market Holidays",
        toolset: "marketdata",
        scope: Scope::ReadMarket,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Get trading holidays for a specific year.

    Args:
        year: Year to get holidays for (e.g., 2026). Optional — defaults to current year.

    Returns:
        JSON with list of trading holidays including:
        - date: Holiday date (YYYY-MM-DD)
        - description: Holiday name/reason
        - holiday_type: TRADING_HOLIDAY, SETTLEMENT_HOLIDAY, or SPECIAL_SESSION
        - closed_exchanges: List of closed exchanges
        - open_exchanges: List of exchanges with special timings

    Example:
        get_holidays(2026)
        get_holidays()          # current year"#,
        params: &[Param {
            name: "year",
            ty: OptInt,
            default: D::Null,
        }],
    },
    ToolDef {
        name: "get_timings",
        title: "Get Market Timings",
        toolset: "marketdata",
        scope: Scope::ReadMarket,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Get exchange trading timings for a specific date.

    Args:
        date: Date in YYYY-MM-DD format (e.g., '2026-04-23'). Optional — defaults to today.

    Returns:
        JSON with exchange timings including:
        - exchange: Exchange name (NSE, BSE, NFO, BFO, MCX, CDS, BCD)
        - start_time: Market open time in epoch milliseconds
        - end_time: Market close time in epoch milliseconds

    Example:
        get_timings("2026-04-23")
        get_timings()           # today"#,
        params: &[Param {
            name: "date",
            ty: OptStr,
            default: D::Null,
        }],
    },
    ToolDef {
        name: "check_holiday",
        title: "Check Trading Holiday",
        toolset: "marketdata",
        scope: Scope::ReadMarket,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Check if a specific date is a market holiday for an exchange.

    This calls the /api/v1/checkholiday endpoint directly (not yet in the openalgo SDK).
    Use this for fast pre-trade "is the market open?" checks.

    Args:
        date: Date in YYYY-MM-DD format (between 2020-01-01 and 2050-12-31). Required.
        exchange: Exchange code (NSE, BSE, NFO, BFO, MCX, CDS, BCD). Optional.
                  When omitted, returns true if the date is a holiday for any major exchange.

    Returns:
        JSON with:
        - status: 'success' or 'error'
        - data.date, data.exchange (if specified), data.is_holiday (bool)

    Notes:
        - Weekends and national holidays both return is_holiday=true.
        - For a full calendar, use get_holidays(year).

    Examples:
        check_holiday("2026-01-26", "NSE")
        check_holiday("2026-01-27")"#,
        params: &[
            Param {
                name: "date",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "exchange",
                ty: OptStr,
                default: D::Null,
            },
        ],
    },
    ToolDef {
        name: "get_instruments",
        title: "Get Instrument Master",
        toolset: "marketdata",
        scope: Scope::ReadMarket,
        risk: Risk::ExternalText,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Download the full instrument master.

    Args:
        exchange: Exchange name (NSE, BSE, NFO, BFO, MCX, CDS, BCD, NSE_INDEX, BSE_INDEX).
                  Optional — when omitted, downloads instruments for ALL exchanges.
        limit: Maximum number of rows to return in the response (default: 500).
               The full dataset can exceed 100k rows for derivatives exchanges, which
               overwhelms the MCP tool output. Use search_instruments for targeted lookups.

    Returns:
        JSON with count, returned, truncated flag, and data (list of instrument records).
        Each record includes: symbol, brsymbol, name, exchange, lotsize,
        instrumenttype, expiry, strike, token, tick_size."#,
        params: &[
            Param {
                name: "exchange",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "limit",
                ty: Int,
                default: D::Int(500),
            },
        ],
    },
    ToolDef {
        name: "analyzer_status",
        title: "Get Analyzer Status",
        toolset: "account",
        scope: Scope::ReadAccount,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Get the current analyzer status including mode and total logs.

    Returns:
        JSON with analyzer status information:
        - data.analyze_mode: Boolean indicating if analyzer is active
        - data.mode: Current mode ('analyze' or 'live')
        - data.total_logs: Number of logs in analyzer
        - status: 'success' or 'error'"#,
        params: &[],
    },
    ToolDef {
        name: "analyzer_toggle",
        title: "Toggle Analyzer Mode",
        toolset: "orders",
        scope: Scope::WriteOrders,
        risk: Risk::BrokerStructured,
        write: true,
        destructive: true,
        open_world: true,
        description: r#"Toggle the analyzer mode between analyze (simulated) and live trading.

    Args:
        mode: True for analyze mode (simulated), False for live mode

    Returns:
        JSON with updated analyzer status:
        - data.analyze_mode, data.message, data.mode, data.total_logs
        - status: 'success' or 'error'

    Example:
        analyzer_toggle(True)  # Switch to analyze mode (simulated responses)
        analyzer_toggle(False) # Switch to live trading mode"#,
        params: &[Param {
            name: "mode",
            ty: Bool,
            default: D::Required,
        }],
    },
    ToolDef {
        name: "calculate_indicator",
        title: "Calculate Indicator",
        toolset: "research",
        scope: Scope::ReadMarket,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Run ANY of the 80+ openalgo.ta indicators over a symbol's historical OHLCV.

    History is fetched (db/api) and the indicator is computed entirely on the
    OpenAlgo server; only compact results are returned — never the raw OHLCV.

    Args:
        symbol: Stock symbol (e.g., 'RELIANCE', 'NIFTY')
        exchange: Exchange name (NSE, NFO, NSE_INDEX, etc.)
        indicator: ta function name, case-insensitive (e.g., 'rsi','macd','supertrend',
                   'atr','bbands','adx','ema','vwap').
        interval: '1m','3m','5m','10m','15m','30m','1h','D' (default 'D')
        start_date / end_date: YYYY-MM-DD. Optional — when omitted, a lookback window
                   ending today is used.
        params: Extra keyword args for the indicator (e.g., {"period": 14} for rsi;
                {"period": 10, "multiplier": 3} for supertrend;
                {"fast_period": 12, "slow_period": 26, "signal_period": 9} for macd).
        inputs: Ordered list of OHLCV columns to feed the indicator, e.g. ["close"] or
                ["high","low","close"]. Optional — auto-detected for common indicators;
                pass it explicitly if a result errors on inputs.
        bars: Number of most-recent computed rows to return (default 20). The indicator
              is ALWAYS computed server-side over the FULL fetched history; only the last
              `bars` rows (plus latest value and summary stats) are sent back, so the
              payload stays small. Increase only if you explicitly need more rows.
        lookback_bars: Bars of history to load/compute over when dates are omitted
                       (default 252 ≈ one trading year of daily data).
        lookback_days: Alternative calendar-day lookback (e.g., 30 for "last 30 days").
                       Overrides lookback_bars when set.
        source: 'api' (default, broker API) or 'db' (local Historify DuckDB store, which
                supports custom research intervals like 2m/4m/W/M/Q).

    Returns:
        JSON with the latest value(s), summary stats (last/min/max/mean), and a 'data'
        series of the last `bars` rows — all computed server-side. Multi-output indicators
        (macd, bbands, supertrend, stochastic, adx, ichimoku, keltner, donchian) report
        out0, out1, ..."#,
        params: &[
            Param {
                name: "symbol",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "exchange",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "indicator",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "interval",
                ty: Str,
                default: D::Str("D"),
            },
            Param {
                name: "start_date",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "end_date",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "params",
                ty: OptObject,
                default: D::Null,
            },
            Param {
                name: "inputs",
                ty: OptStrList,
                default: D::Null,
            },
            Param {
                name: "bars",
                ty: Int,
                default: D::Int(20),
            },
            Param {
                name: "lookback_bars",
                ty: Int,
                default: D::Int(252),
            },
            Param {
                name: "lookback_days",
                ty: OptInt,
                default: D::Null,
            },
            Param {
                name: "source",
                ty: Str,
                default: D::Str("api"),
            },
        ],
    },
    ToolDef {
        name: "get_trend_snapshot",
        title: "Get Trend Snapshot",
        toolset: "research",
        scope: Scope::ReadMarket,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"One-call trend read: SMA(20/50/200), EMA(20/50), Supertrend, ADX/DMI, Ichimoku.

    Args:
        symbol: Stock symbol
        exchange: Exchange name
        interval: Candle interval (default 'D')
        start_date / end_date: YYYY-MM-DD. Optional — default to a lookback window ending today.
        lookback_bars: Bars of history loaded when dates are omitted (default 252, enough for SMA200).
        lookback_days: Alternative calendar-day lookback (e.g., 30). Overrides lookback_bars.

    Returns:
        JSON with latest indicator values and a 'legend' explaining multi-value entries."#,
        params: &[
            Param {
                name: "symbol",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "exchange",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "interval",
                ty: Str,
                default: D::Str("D"),
            },
            Param {
                name: "start_date",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "end_date",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "lookback_bars",
                ty: Int,
                default: D::Int(252),
            },
            Param {
                name: "lookback_days",
                ty: OptInt,
                default: D::Null,
            },
            Param {
                name: "source",
                ty: Str,
                default: D::Str("api"),
            },
        ],
    },
    ToolDef {
        name: "get_momentum_snapshot",
        title: "Get Momentum Snapshot",
        toolset: "research",
        scope: Scope::ReadMarket,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"One-call momentum read: RSI(14), MACD, Stochastic, CCI(20), Williams %R(14).

    Args:
        symbol: Stock symbol
        exchange: Exchange name
        interval: Candle interval (default 'D')
        start_date / end_date: YYYY-MM-DD. Optional — default to a lookback window ending today.
        lookback_bars: Bars of history loaded when dates are omitted (default 252).
        lookback_days: Alternative calendar-day lookback (e.g., 30). Overrides lookback_bars.

    Returns:
        JSON with latest values and a 'legend' for multi-value entries."#,
        params: &[
            Param {
                name: "symbol",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "exchange",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "interval",
                ty: Str,
                default: D::Str("D"),
            },
            Param {
                name: "start_date",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "end_date",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "lookback_bars",
                ty: Int,
                default: D::Int(252),
            },
            Param {
                name: "lookback_days",
                ty: OptInt,
                default: D::Null,
            },
            Param {
                name: "source",
                ty: Str,
                default: D::Str("api"),
            },
        ],
    },
    ToolDef {
        name: "get_volatility_snapshot",
        title: "Get Volatility Snapshot",
        toolset: "research",
        scope: Scope::ReadMarket,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"One-call volatility read: ATR, NATR, Bollinger Bands (+%B, width), Keltner,
    Donchian, Historical Volatility.

    Args:
        symbol: Stock symbol
        exchange: Exchange name
        interval: Candle interval (default 'D')
        start_date / end_date: YYYY-MM-DD. Optional — default to a lookback window ending today.
        lookback_bars: Bars of history loaded when dates are omitted (default 252).
        lookback_days: Alternative calendar-day lookback (e.g., 30). Overrides lookback_bars.

    Returns:
        JSON with latest values and a 'legend' for multi-value band entries."#,
        params: &[
            Param {
                name: "symbol",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "exchange",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "interval",
                ty: Str,
                default: D::Str("D"),
            },
            Param {
                name: "start_date",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "end_date",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "lookback_bars",
                ty: Int,
                default: D::Int(252),
            },
            Param {
                name: "lookback_days",
                ty: OptInt,
                default: D::Null,
            },
            Param {
                name: "source",
                ty: Str,
                default: D::Str("api"),
            },
        ],
    },
    ToolDef {
        name: "get_support_resistance",
        title: "Get Support and Resistance",
        toolset: "research",
        scope: Scope::ReadMarket,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Support/resistance levels: Pivot Points, Donchian channel, and rolling
    highest-high / lowest-low over `period`.

    Args:
        symbol: Stock symbol
        exchange: Exchange name
        interval: Candle interval (default 'D')
        start_date / end_date: YYYY-MM-DD. Optional — default to a lookback window ending today.
        lookback_bars: Bars of history loaded when dates are omitted (default 252).
        lookback_days: Alternative calendar-day lookback (e.g., 30). Overrides lookback_bars.
        period: Lookback window for Donchian / highest / lowest (default 20).

    Returns:
        JSON with latest levels. 'pivot_points' is returned as ta.pivot_points emits it
        (typically [pivot, r1, s1, r2, s2, r3, s3])."#,
        params: &[
            Param {
                name: "symbol",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "exchange",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "interval",
                ty: Str,
                default: D::Str("D"),
            },
            Param {
                name: "start_date",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "end_date",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "lookback_bars",
                ty: Int,
                default: D::Int(252),
            },
            Param {
                name: "lookback_days",
                ty: OptInt,
                default: D::Null,
            },
            Param {
                name: "period",
                ty: Int,
                default: D::Int(20),
            },
            Param {
                name: "source",
                ty: Str,
                default: D::Str("api"),
            },
        ],
    },
    ToolDef {
        name: "detect_signals",
        title: "Detect Signals",
        toolset: "research",
        scope: Scope::ReadMarket,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Detect technical signals over a symbol's history using ta crossover/threshold logic.

    Args:
        symbol: Stock symbol
        exchange: Exchange name
        interval: Candle interval (default 'D')
        start_date / end_date: YYYY-MM-DD. Optional — default to the lookback window.
        lookback_bars: Bars loaded when dates omitted (default 252).
        lookback_days: Alternative calendar-day lookback (e.g., 30). Overrides lookback_bars.
        signal_type: One of:
            'ema_cross'      - EMA(fast) crossing EMA(slow)
            'sma_cross'      - SMA(fast) crossing SMA(slow)
            'macd_cross'     - MACD line crossing its signal line
            'supertrend_flip'- Supertrend direction flip
            'rsi_threshold'  - RSI crossing out of oversold(lower) / overbought(upper)
        fast / slow: MA periods for ema_cross / sma_cross
        period: Lookback for rsi_threshold (default 14)
        upper / lower: RSI overbought / oversold levels (default 70 / 30)
        limit: Max number of most-recent signal events to return (default 20)

    Returns:
        JSON with recent events [{timestamp, signal: 'bullish'|'bearish'}] plus current values."#,
        params: &[
            Param {
                name: "symbol",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "exchange",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "interval",
                ty: Str,
                default: D::Str("D"),
            },
            Param {
                name: "start_date",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "end_date",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "signal_type",
                ty: Str,
                default: D::Str("ema_cross"),
            },
            Param {
                name: "fast",
                ty: Int,
                default: D::Int(20),
            },
            Param {
                name: "slow",
                ty: Int,
                default: D::Int(50),
            },
            Param {
                name: "period",
                ty: Int,
                default: D::Int(14),
            },
            Param {
                name: "upper",
                ty: Num,
                default: D::Num(70.0),
            },
            Param {
                name: "lower",
                ty: Num,
                default: D::Num(30.0),
            },
            Param {
                name: "limit",
                ty: Int,
                default: D::Int(20),
            },
            Param {
                name: "lookback_bars",
                ty: Int,
                default: D::Int(252),
            },
            Param {
                name: "lookback_days",
                ty: OptInt,
                default: D::Null,
            },
            Param {
                name: "source",
                ty: Str,
                default: D::Str("api"),
            },
        ],
    },
    ToolDef {
        name: "screen_instruments",
        title: "Screen Instruments",
        toolset: "research",
        scope: Scope::ReadMarket,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Scan a watchlist of symbols for a technical condition.

    Note: this fetches history per symbol sequentially — keep the list modest (≤ ~25)
    or use a coarse interval to bound runtime and broker API calls.

    Args:
        symbols: List of {"symbol","exchange"} pairs.
            Example: [{"symbol":"RELIANCE","exchange":"NSE"},{"symbol":"INFY","exchange":"NSE"}]
        interval: Candle interval (default 'D')
        start_date / end_date: YYYY-MM-DD. Optional — default to the lookback window.
        lookback_bars: Bars loaded per symbol when dates omitted (default 252).
        lookback_days: Alternative calendar-day lookback (e.g., 30). Overrides lookback_bars.
        condition: One of:
            'rsi_below' / 'rsi_above'        - RSI(period) vs `value`
            'price_above_sma'/'price_below_sma' - last close vs SMA(period)
            'supertrend_bullish'/'supertrend_bearish' - current Supertrend direction
        value: Threshold for rsi conditions (default 30)
        period: Lookback for rsi / sma (default 14)

    Returns:
        JSON with per-symbol {passed, metric} and a count of matches."#,
        params: &[
            Param {
                name: "symbols",
                ty: StrMapList,
                default: D::Required,
            },
            Param {
                name: "interval",
                ty: Str,
                default: D::Str("D"),
            },
            Param {
                name: "start_date",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "end_date",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "condition",
                ty: Str,
                default: D::Str("rsi_below"),
            },
            Param {
                name: "value",
                ty: Num,
                default: D::Num(30.0),
            },
            Param {
                name: "period",
                ty: Int,
                default: D::Int(14),
            },
            Param {
                name: "lookback_bars",
                ty: Int,
                default: D::Int(252),
            },
            Param {
                name: "lookback_days",
                ty: OptInt,
                default: D::Null,
            },
            Param {
                name: "source",
                ty: Str,
                default: D::Str("api"),
            },
        ],
    },
    ToolDef {
        name: "multi_timeframe_analysis",
        title: "Multi-Timeframe Analysis",
        toolset: "research",
        scope: Scope::ReadMarket,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Compute the same indicator across multiple timeframes for confluence analysis.

    Args:
        symbol: Stock symbol
        exchange: Exchange name
        start_date / end_date: YYYY-MM-DD. Optional — default to the lookback window per interval.
        intervals: List of intervals (default ['5m','15m','1h','D'])
        indicator: ta function name (default 'rsi')
        params: Extra keyword args for the indicator (e.g., {"period": 14})
        inputs: Ordered input columns; auto-detected if omitted.
        lookback_bars: Bars loaded per interval when dates omitted (default 252).
        lookback_days: Alternative calendar-day lookback (e.g., 30). Overrides lookback_bars.

    Returns:
        JSON with the latest indicator value (and last_close) per timeframe."#,
        params: &[
            Param {
                name: "symbol",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "exchange",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "start_date",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "end_date",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "intervals",
                ty: OptStrList,
                default: D::Null,
            },
            Param {
                name: "indicator",
                ty: Str,
                default: D::Str("rsi"),
            },
            Param {
                name: "params",
                ty: OptObject,
                default: D::Null,
            },
            Param {
                name: "inputs",
                ty: OptStrList,
                default: D::Null,
            },
            Param {
                name: "lookback_bars",
                ty: Int,
                default: D::Int(252),
            },
            Param {
                name: "lookback_days",
                ty: OptInt,
                default: D::Null,
            },
            Param {
                name: "source",
                ty: Str,
                default: D::Str("api"),
            },
        ],
    },
    ToolDef {
        name: "correlation_beta",
        title: "Correlation and Beta",
        toolset: "research",
        scope: Scope::ReadMarket,
        risk: Risk::BrokerStructured,
        write: false,
        destructive: false,
        open_world: true,
        description: r#"Correlation / Beta / Linear-regression slope between two symbols (pairs & hedge research).

    Both symbols' closes are aligned on common timestamps before computing.

    Args:
        symbol1 / exchange1: First instrument (the 'asset')
        symbol2 / exchange2: Second instrument (the 'market'/benchmark)
        interval: Candle interval (default 'D')
        start_date / end_date: YYYY-MM-DD. Optional — default to the lookback window.
        period: Rolling window for correlation/beta/slope (default 20)
        lookback_bars: Bars loaded per symbol when dates omitted (default 252).
        lookback_days: Alternative calendar-day lookback (e.g., 30). Overrides lookback_bars.

    Returns:
        JSON with rolling correlation, rolling beta, LR slope of symbol1, the full-sample
        Pearson correlation, and the number of overlapping bars."#,
        params: &[
            Param {
                name: "symbol1",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "exchange1",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "symbol2",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "exchange2",
                ty: Str,
                default: D::Required,
            },
            Param {
                name: "interval",
                ty: Str,
                default: D::Str("D"),
            },
            Param {
                name: "start_date",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "end_date",
                ty: OptStr,
                default: D::Null,
            },
            Param {
                name: "period",
                ty: Int,
                default: D::Int(20),
            },
            Param {
                name: "lookback_bars",
                ty: Int,
                default: D::Int(252),
            },
            Param {
                name: "lookback_days",
                ty: OptInt,
                default: D::Null,
            },
            Param {
                name: "source",
                ty: Str,
                default: D::Str("api"),
            },
        ],
    },
];
