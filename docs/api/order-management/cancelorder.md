# CancelOrder

Cancel one working order by its order id.

## Endpoint URL

```http
POST http://127.0.0.1:5000/api/v1/cancelorder
```

## Sample API Request

```json
{
  "apikey": "<APIKEY>",
  "orderid": "250408001002736",
  "strategy": "Python"
}
```

## Sample cURL Request

```bash
curl -X POST http://127.0.0.1:5000/api/v1/cancelorder \
  -H 'Content-Type: application/json' \
  -d '{
  "apikey": "<APIKEY>",
  "orderid": "250408001002736",
  "strategy": "Python"
}'
```

## Sample API Response

```json
{
  "orderid": "250408001002736",
  "status": "success"
}
```

## Sample API Response (Analyzer Mode)

```json
{
  "message": "Order cancelled successfully",
  "mode": "analyze",
  "orderid": "26100348649169",
  "status": "success"
}
```

## Request Body

| Parameter | Description | Mandatory/Optional | Default Value |
|-----------|-------------|-------------------|---------------|
| apikey | Your OpenAlgo API key (1 to 256 characters) | Mandatory | - |
| orderid | The id of the order to cancel | Mandatory | - |
| strategy | Strategy name | Mandatory | - |

`strategy` is required by the schema, as on the web (the web page lists it as optional). Any other field is refused as `Unknown field.`

## Response Fields

| Field | Type | Description |
|-------|------|-------------|
| status | string | `success` or `error` |
| orderid | string | The order that was cancelled |
| message | string | `Order cancelled successfully` in analyzer mode, or the reason for an error |
| mode | string | `analyze` when analyzer mode is on; absent in live mode |

## Analyzer Mode

With analyzer mode on, the sandbox cancels its own order, releases the margin the order held, and the answer carries `"mode": "analyze"`. Both `open` and `trigger pending` orders (resting SL and SL-M orders) can be cancelled. See [Sandbox mode](../../user/sandbox.md).

## Errors

Validation errors carry `message` as a string that spells out a Python dictionary. In analyzer mode the body also carries `"mode": "analyze"`:

```json
{
  "message": "{'orderid': ['Missing data for required field.']}",
  "mode": "analyze",
  "status": "error"
}
```

| HTTP | `message` | When |
|------|-----------|------|
| 400 | `{'orderid': ['Missing data for required field.']}` | A required field is missing |
| 400 | `Order ID is missing` | `orderid` is an empty string |
| 400 | The broker's own reason | The broker refused the cancel |
| 429 | `OpenAlgo is pacing requests to stay within your broker's rate limit, and this one would have had to wait too long for its turn. Try again in a few seconds.` | The cancel would have waited more than 10 seconds for its turn at the broker; it was not sent |
| 403 | `Cancel order operation is not allowed in Semi-Auto mode. Please switch to Auto mode to cancel orders.` | The API key's order mode is Semi-Auto (live mode only) |
| 500 | `Failed to cancel order due to internal error` | No usable answer from the broker |

In analyzer mode the sandbox adds its own refusals, with `"mode": "analyze"`:

| HTTP | `message` | When |
|------|-----------|------|
| 404 | `Order 99999999999999 not found` | No sandbox order has that id |
| 400 | `Cannot cancel order in complete status` | The order has filled |
| 400 | `Cannot cancel order in cancelled status` | The order was already cancelled |

Errors common to every endpoint (missing or invalid API key, OpenAlgo locked, wrong method, rate limit) are listed in the [API overview](../README.md#errors-common-to-every-endpoint).

## Notes

- Only working orders (open or trigger pending) can be cancelled. An order that has filled cannot.
- An order that is already cancelled answers with an error in analyzer mode. What your broker answers for it depends on the broker.
- Cancel is never queued in the Action Center. In Semi-Auto it is refused in live mode and allowed in analyzer mode, as on the web.
- Rate limit: 10 requests per second per caller, shared with the other order endpoints. See [Rate limiting](../rate-limiting.md).

## Related Endpoints

- [CancelAllOrder](./cancelallorder.md): cancel every working order at once

## In the desktop

- A cancel the broker pacer cannot send within 10 seconds is answered 429, as on the web; the desktop always bounds the wait, the web only on a multi-threaded server.
- In the desktop, if the order mode (Auto or Semi-Auto) cannot be read, a live cancel is refused with 500 and nothing is sent; the web treats an unreadable mode as Auto.

---

**Back to**: [API Documentation](../README.md)
