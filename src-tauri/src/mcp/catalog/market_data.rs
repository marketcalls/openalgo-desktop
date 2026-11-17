//! The `marketdata` toolset: quotes, depth, history, symbols, expiries,
//! option symbols and Greeks, and the market calendar.

use crate::mcp::schema::{Dflt as D, Param, Ty::*};
use crate::mcp::{Risk, Scope, ToolDef};

pub const TOOLS: &[ToolDef] = &[
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
        symbol: OpenAlgo symbol, e.g. RELIANCE, NIFTY, SENSEX, NIFTY28MAR2420800CE
        exchange: NSE, BSE, NFO, BFO, MCX, CDS, BCD, NSE_INDEX or BSE_INDEX.
            Optional. When omitted, an index uses its index exchange (SENSEX,
            BANKEX and SENSEX50 use BSE_INDEX; NIFTY, BANKNIFTY and the other
            NSE indices use NSE_INDEX) and any other symbol uses NSE. Pass it
            for BSE equities and for every derivative contract."#,
        params: &[
            Param {
                name: "symbol",
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
        symbol: Underlying symbol, e.g. NIFTY, BANKNIFTY, SENSEX, RELIANCE
        exchange: Derivatives exchange: NFO, BFO, MCX or CDS. Optional. When
            omitted, SENSEX, BANKEX and SENSEX50 use BFO and any other
            underlying uses NFO. Pass MCX or CDS for commodity and currency
            underlyings.
        instrument_type: 'options' or 'futures'"#,
        params: &[
            Param {
                name: "symbol",
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
];
