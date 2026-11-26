# Order Updates (WebSocket)

Receive your orders' status changes as they happen: placed, filled,
rejected, cancelled. The broker sends them for live orders; the sandbox sends
them in analyzer mode. This is an account-level stream: no symbols or modes,
one subscription covers every order of the logged-in account, whoever placed
it (the API, the app, the broker's own app or website).

Connecting and authenticating are described in
[WebSocket Streaming](./README.md).

## WebSocket URL

```
ws://127.0.0.1:8765
```

## Subscribe to Order Updates

Authenticate first, then send:

```json
{
  "action": "subscribe_orders",
  "request_id": "orders-1"
}
```

### Subscribe Acknowledgement

```json
{
  "type": "subscribe_orders",
  "status": "success",
  "message": "Subscribed to order updates",
  "request_id": "orders-1"
}
```

## Order Update Message

Every update has the same eighteen fields, in OpenAlgo's common order
constants: `symbol` in OpenAlgo format (never the broker's own symbol),
`action` `BUY` or `SELL`, `pricetype` `MARKET`, `LIMIT`, `SL` or `SL-M`,
`product` `CNC`, `NRML` or `MIS`, and a lowercase `order_status` (`open`,
`trigger pending`, `complete`, `rejected`, `cancelled`, plus broker extras
such as `expired`).

A sandbox fill in analyzer mode (illustrative values):

```json
{
  "type": "order_update",
  "user_id": "<USER_ID>",
  "mode": "analyze",
  "broker": "sandbox",
  "orderid": "26100376733573",
  "symbol": "NIFTY27OCT26FUT",
  "exchange": "NFO",
  "action": "BUY",
  "quantity": 65,
  "price": 22520.0,
  "trigger_price": 0.0,
  "pricetype": "MARKET",
  "product": "NRML",
  "order_status": "complete",
  "filled_quantity": 65,
  "pending_quantity": 0,
  "average_price": 22520.0,
  "rejection_reason": ""
}
```

A live rejection pushed by the broker (illustrative values):

```json
{
  "type": "order_update",
  "user_id": "<USER_ID>",
  "mode": "live",
  "broker": "upstox",
  "orderid": "260715000344871",
  "symbol": "NHPC",
  "exchange": "NSE",
  "action": "BUY",
  "quantity": 1,
  "price": 50.0,
  "trigger_price": 0.0,
  "pricetype": "LIMIT",
  "product": "CNC",
  "order_status": "rejected",
  "filled_quantity": 0,
  "pending_quantity": 0,
  "average_price": 0.0,
  "rejection_reason": "RMS:Rule: Check circuit limit including square off order exceeds : Circuit breach ..."
}
```

### Fields

| Field | Type | Description |
|-------|------|-------------|
| type | string | Always `order_update` |
| user_id | string | Your OpenAlgo user name |
| mode | string | `live` for broker updates, `analyze` for sandbox updates |
| broker | string | The broker, or `sandbox` |
| orderid | string | Order id |
| symbol | string | Symbol in OpenAlgo format |
| exchange | string | Exchange code |
| action | string | `BUY` or `SELL` |
| quantity | number | Order quantity |
| price | number | Order price; for a sandbox fill, the fill price |
| trigger_price | number | Trigger price; 0 when none |
| pricetype | string | `MARKET`, `LIMIT`, `SL` or `SL-M` |
| product | string | `CNC`, `NRML` or `MIS` |
| order_status | string | Lowercase status |
| filled_quantity | number | Quantity filled so far |
| pending_quantity | number | Quantity still open |
| average_price | number | Average fill price; 0 before any fill |
| rejection_reason | string | Why it was rejected; empty otherwise |

## Unsubscribe

```json
{"action": "unsubscribe_orders"}
```

```json
{
  "type": "unsubscribe_orders",
  "status": "success",
  "message": "Unsubscribed from order updates"
}
```

## Where Updates Come From

- **The broker's own order stream** for Zerodha, Fyers, Upstox, Dhan, Kotak,
  Angel, AliceBlue, Arrow, Definedge, IIFL Capital, INDmoney, Motilal, Nubra,
  Nuvama, Pocketful, Shoonya, Flattrade, Zebu, TradeSmart and Tradejini.
- **Polling the order book** for Groww, Samco and 5paisa, which have no push
  stream: changes are found by reading the order book every few seconds.
- **The sandbox** in analyzer mode: placements, fills, rejections and
  cancellations. No broker order is needed to test end to end.

Other brokers send no live order updates; read their orders with
[OrderStatus](../order-information/orderstatus.md) or the
[OrderBook](../account-services/orderbook.md).

The stream starts when you log in to your broker and stops when you log out
or switch brokers. A connection that authenticated before the broker session
changed is closed (4401) and must authenticate again; see
[How a Connection Ends](./README.md#how-a-connection-ends).

## Desktop Differences

- There are no `/postback/<broker>` receivers for brokers' HTTPS postbacks.
  Updates come from the sources above only.
- There are no settings files: order updates cannot be turned off with
  `ORDER_UPDATES_ENABLED`, and there is no `ORDER_POLL_INTERVAL` to set.

## Notes

- Updates are not stored for the connection: one sent while you were not
  subscribed is not sent again. After reconnecting, read the order book once.
- To tell repeated updates of the same order apart, compare `orderid`,
  `order_status` and `filled_quantity`.

---

**Back to**: [API Documentation](../README.md)
