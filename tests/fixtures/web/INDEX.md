# OpenAlgo golden fixtures (captured 2026-10-03, Saturday, market closed)

Source instance: http://127.0.0.1:5000 (Flask/Werkzeug dev server), ws://127.0.0.1:8765, broker zerodha.
All API keys replaced by `<APIKEY>`; `user_id`/client ids by `<USER_ID>`; email by `<EMAIL>`. Numeric market data unchanged.
IMPORTANT: `/api/v1/analyzer` reported `analyze_mode: true` at capture time, so account endpoints (funds/orderbook/tradebook/positionbook/holdings/gttorderbook/openposition/orderstatus/pnl) are SANDBOX-shaped and carry `"mode": "analyze"`. Session 1 called no state-changing endpoint. Session 2 (04:11-04:16 UTC, user-authorised) placed/modified/cancelled orders ONLY in analyze mode and did one toggle round trip (analyze -> live -> analyze); final mode analyze. Rows marked S2 are from session 2; see ANALYZER_SESSION.md.

## REST fixtures (`rest/<endpoint>/<case>.json`)

Each file: `{request:{method,path,headers,body}, response:{status_code,headers,body}, note?}`. Large arrays are truncated with `_total_rows`/`_truncated` markers.

| Endpoint | Case | HTTP | Note |
|---|---|---|---|
| errors | bad_exchange_quotes | 400 | 400 lists all valid exchanges: NSE, NFO, CDS, BSE, BFO, BCD, MCX, NCDEX, NCO, NSE_INDEX, BSE_INDEX, MCX_INDEX, GLOBAL_INDEX, CRYPTO |
| errors | empty_apikey_string | 400 | 400 'Length must be between 1 and 256.' |
| errors | empty_body_json_content_type | 400 | 400 Werkzeug generic message, no status field |
| errors | invalid_apikey_funds | 403 | 403 {status:'error', message:'Invalid openalgo apikey'} |
| errors | invalid_apikey_quotes | 403 |  |
| errors | json_array_body | 500 | 500 bare {message:'Internal Server Error'} (flask-restx) -- no status field |
| errors | lowercase_exchange_quotes | 400 | exchange is case-sensitive |
| errors | malformed_json_quotes | 400 | 400 Werkzeug body {message:'The browser (or proxy) sent a request...'} with NO status field |
| errors | missing_apikey_funds | 400 | 400 message is an OBJECT {field:[msgs]} (marshmallow) not a string |
| errors | missing_apikey_quotes | 400 |  |
| errors | missing_required_field_history_interval | 400 |  |
| errors | missing_required_field_quotes_symbol | 400 |  |
| errors | non_json_content_type | 500 | 500 {status:'error', message:'An unexpected error occurred'} (request.json None) |
| errors | rate_limit_429_placeorder | 429 | S2 ORDER_RATE_LIMIT 10/s: 429 {message:'10 per 1 second'} -- no status field, no Retry-After / X-RateLimit headers (ORDER_RATE_LIMIT 10/s hit by 13 rapid placeorder calls: 429 {message:'10 per 1 second'} -- no status field, no Retry-After/X-RateLimit headers) |
| errors | rate_limit_probe_no_429 | 200 | 103 pings in 2.6s -> all 200; limit is 100/SECOND (not per minute) so single-client burst could not exceed it; no X-RateLimit headers emitted (burst of 103 pings in 2.618s produced no 429; counts={200: 103}) |
| errors | trailing_slash_ping | 200 | /ping/ accepted (strict_slashes=False) |
| errors | unknown_route | 404 | 404 {status:'error', message:'Not found', path:'/api/v1/doesnotexist'} |
| errors | unknown_route_outside_api | 404 | 404 with text/html SPA index shell (not JSON) -- only /api/v1/* 404s are JSON |
| errors | unknown_route_post | 404 |  |
| errors | unknown_symbol_depth | 400 |  |
| errors | unknown_symbol_quotes | 400 | 400 string message 'Symbol ... not found for exchange ...' |
| errors | wrong_method_get_on_funds | 404 | GET on POST endpoint -> 404 (not 405) {status:'error', message:'Not found', path} |
| errors | wrong_method_get_on_quotes | 404 |  |
| errors | wrong_method_put_on_ping | 404 | 404 as above |
| analyzer | status | 200 | data:{analyze_mode:true, mode:'analyze', total_logs}; instance was in analyze mode at capture time. /analyzer/toggle NOT called (POST /analyzer returns mode status; /analyzer/toggle NOT called) |
| analyzer | status_after_round_trip | 200 | S2  |
| analyzer | status_before_mutations | 200 | S2  |
| analyzer | status_final | 200 | S2  |
| analyzer | status_while_live | 200 | S2  |
| analyzer | toggle_back_to_analyze_true | 200 | S2 message 'Analyzer mode switched to analyze' |
| analyzer | toggle_error_invalid_mode | 400 | S2 400 stringified dict, no mode key |
| analyzer | toggle_error_missing_mode | 400 | S2  |
| analyzer | toggle_to_live_false | 200 | S2 data:{analyze_mode:false, message:'Analyzer mode switched to live', mode:'live', total_logs} (round trip for shape only; no orders placed while live) |
| basketorder | error_empty_orders | 400 | S2  |
| basketorder | error_leg_bad_product | 400 | S2 nested stringified dict "{'orders': {0: {'product': [...]}}}" |
| basketorder | one_leg_unknown_symbol | 200 | S2 overall status 'success' HTTP 200; failed leg {message, status:'error', symbol} |
| basketorder | three_legs_mixed | 200 | S2 results:[{batch_order:true, exchange, is_last_order, orderid, product, status, symbol}] |
| cancelallorder | error_missing_strategy | 400 | S2  |
| cancelallorder | final_cleanup | 200 | S2  |
| cancelallorder | nothing_open | 200 | S2 200 success, message 'No open orders to cancel', empty lists |
| cancelallorder | with_open_orders | 200 | S2 {canceled_orders:[ids], failed_cancellations:[], message:'Canceled N orders. Failed to cancel M orders.', mode}. BUG: skips 'trigger pending' orders (filter checks 'trigger_pending') |
| cancelgttorder | error_already_cancelled | 404 | S2 404 "No active GTT with trigger_id '...'" |
| cancelgttorder | error_missing_trigger_id | 400 | S2  |
| cancelgttorder | error_unknown_trigger_id | 404 | S2  |
| cancelgttorder | oco | 500 | S2 500 margin-release failure after OCO modify + closeposition: 'Could not release 1106.80 margin ... The GTT is unchanged - retry the cancel.' (retry also fails; GTT left active) |
| cancelgttorder | oco_retry | 500 | S2  |
| cancelgttorder | single | 200 | S2 {status, trigger_id, mode} |
| cancelorder | error_already_cancelled | 400 | S2 400 'Cannot cancel order in cancelled status' |
| cancelorder | error_completed_order | 400 | S2  |
| cancelorder | error_missing_orderid | 400 | S2  |
| cancelorder | error_unknown_orderid | 404 | S2 404 |
| cancelorder | open_limit | 200 | S2 {status, message:'Order cancelled successfully', orderid, mode} |
| cancelorder | trigger_pending_sl | 200 | S2 trigger-pending orders must be cancelled individually (see cancelallorder bug) (cancelallorder skipped this trigger-pending order) |
| cancelorder | trigger_pending_slm | 200 | S2  |
| chart | get_missing_apikey | 400 | 400 'Missing apikey parameter' |
| chart | get_preferences | 200 | GET with apikey query; data:{} when nothing saved |
| closeposition | error_missing_strategy | 400 | S2  |
| closeposition | final_cleanup | 200 | S2  |
| closeposition | no_open_positions | 200 | S2 200 SUCCESS (not an error) 'No open positions to close' |
| closeposition | with_open_positions | 200 | S2 {closed_positions:<int count>, failed_closures:<int>, message:'Closed N positions', mode} |
| depth | crudeoil_future_mcx | 200 |  |
| depth | nifty_future_nfo | 200 |  |
| depth | nifty_nse_index | 200 |  |
| depth | nifty_option_nfo | 200 |  |
| depth | reliance_nse | 200 | data:{asks:[{price,quantity}x5], bids:[...], high, low, ltp, ltq, oi, open, prev_close, totalbuyqty, totalsellqty, volume}; 5 levels of zeros when closed |
| expiry | banknifty_nfo_options | 200 |  |
| expiry | crudeoil_mcx_futures | 200 |  |
| expiry | crudeoil_mcx_futures_session2 | 200 | S2  |
| expiry | nifty_nfo_futures | 200 | data is a flat list of 'DD-MMM-YY' strings (27-OCT-26); NOT the DDMMMYY symbol format |
| expiry | nifty_nfo_futures_session2 | 200 | S2  |
| expiry | nifty_nfo_options | 200 | weekly + monthly expiries, DD-MMM-YY |
| expiry | nifty_nfo_options_session2 | 200 | S2  |
| funds | after_activity | 200 | S2 adds last_reset, reset_count, today_realized_pnl, total_realized_pnl; utiliseddebits == grossexposure |
| funds | after_cleanup | 200 | S2  |
| funds | apikey_in_body | 200 | instance was in ANALYZE mode: sandbox funds (availablecash 1e7), extra 'mode':'analyze', fields are numbers (not strings as in live brokers) |
| funds | apikey_in_header_and_body | 200 |  |
| funds | apikey_in_x_api_key_header | 400 | 400, header not supported |
| funds | final | 200 | S2  |
| gttorderbook | after_all_cancelled | 200 | S2  |
| gttorderbook | after_cancel_all_statuses | 200 | S2  |
| gttorderbook | after_modify | 200 | S2  |
| gttorderbook | after_place | 200 | S2 rows {created_at ISO local no tz, expires_at (+1y), last_price, legs[{action,price,pricetype,product,quantity,triggered_order_id}], margin_blocked, status:'active', strategy, symbol, trigger_id, trigger_prices[], trigger_type:'single'/'two-leg' (request OCO -> 'two-leg'), updated_at} |
| gttorderbook | default | 200 | data:[] + mode:'analyze' |
| gttorderbook | error_bad_status | 400 | S2  |
| gttorderbook | final | 200 | S2  |
| gttorderbook | status_active | 200 | S2  |
| gttorderbook | status_all | 200 | S2 includes cancelled rows (status 'cancelled', margin_blocked 0) |
| gttorderbook | status_cancelled | 400 | S2 status filter only accepts active/all; message is an OBJECT here (read-endpoint style) |
| history | bad_date_format_ddmmyyyy | 400 | dates must be YYYY-MM-DD -> 'Not a valid date.' (Date format must be YYYY-MM-DD) |
| history | bad_interval | 400 | schema allows 1s..Y but broker /intervals only lists a subset |
| history | crudeoil_future_15m | 200 |  |
| history | end_before_start | 200 | 200 with data:[] (no validation) |
| history | nifty_future_5m | 200 |  |
| history | nifty_index_D | 200 |  |
| history | nifty_option_5m | 200 |  |
| history | reliance_nse_15m | 200 | range 2026-09-22..2026-10-02 rows=192 first_ts=1790048700 keys=['close', 'high', 'low', 'oi', 'open', 'timestamp', 'volume'] |
| history | reliance_nse_1m | 200 | data:[{close,high,low,oi,open,timestamp,volume}] timestamp = epoch SECONDS (int); truncated in file, _total_rows recorded (range 2026-09-29..2026-10-02 rows=1080 first_ts=1790653500 keys=['close', 'high', 'low', 'oi', 'open', 'timestamp', 'volume']) |
| history | reliance_nse_5m | 200 | range 2026-09-29..2026-10-02 rows=216 first_ts=1790653500 keys=['close', 'high', 'low', 'oi', 'open', 'timestamp', 'volume'] |
| history | reliance_nse_D | 200 | daily candles, timestamp epoch seconds (range 2026-08-01..2026-10-02 rows=43 first_ts=1785715200 keys=['close', 'high', 'low', 'oi', 'open', 'timestamp', 'volume']) |
| history | reliance_nse_D_source_db | 404 | source='db' -> 404 (no local DB copy) |
| holdings | after_cnc_buys | 200 | S2 same-day CNC buys do not appear in holdings (T+1 settlement in sandbox) |
| holdings | default | 200 | data:{holdings:[], statistics:{totalholdingvalue,...}} + mode |
| instruments | all_exchanges_json | 400 | exchange is effectively required: 400 'Field may not be null.' |
| instruments | bad_exchange | 400 |  |
| instruments | nfo_json | 200 |  |
| instruments | nse_csv | 200 | text/csv attachment; header row symbol,brsymbol,name,exchange,brexchange,token,expiry,strike,lotsize,instrumenttype,tick_size |
| instruments | nse_json | 200 | GET only; apikey in query string; data truncated to 20 rows, _total_rows kept (GET with query params; data truncated to 20 rows, _total_rows recorded) |
| instruments | post_not_allowed | 404 | POST -> 404 (not 405) (instruments is GET-only) |
| intervals | default | 200 | data:{seconds:[],minutes:[...],hours:['1h'],days:['D'],weeks:[],months:[]} |
| margin | basket_fut_and_option | 200 | lot size 65 from /search |
| margin | empty_positions | 400 | 400 |
| margin | quantity_as_number_not_string | 400 | numeric quantity rejected: 400 'Not a valid string.' (schema declares quantity/price as Str) |
| margin | single_equity_mis | 200 | quantity/price are STRINGS in request; data:{exposure_margin, span_margin, total_margin_required} numbers |
| margin | single_option_limit | 200 |  |
| market/holidays | no_year | 200 |  |
| market/holidays | year_2026 | 200 | data:[{date 'YYYY-MM-DD', description, holiday_type, closed_exchanges[], open_exchanges:[{exchange,start_time,end_time ms}]}] |
| market/holidays | year_out_of_range | 400 |  |
| market/timings | bad_date | 400 |  |
| market/timings | saturday_2026-10-03 | 200 | only CRYPTO open on Saturday; start_time/end_time epoch MILLISECONDS |
| market/timings | weekday_2026-10-01 | 200 | per-exchange ms timestamps |
| modifygttorder | error_cancelled_trigger | 500 | S2 BUG 500, same cause |
| modifygttorder | error_missing_trigger_id | 400 | S2  |
| modifygttorder | error_unknown_trigger_id | 500 | S2 BUG 500 'An unexpected error occurred': GTTModifyFailedEvent has no 'exchange' field -> TypeError on any failed sandbox modify |
| modifygttorder | oco_change_target | 200 | S2  |
| modifygttorder | single_change_trigger_and_qty | 200 | S2 {status, trigger_id, mode} |
| modifyorder | error_cancelled_order | 400 | S2  |
| modifyorder | error_completed_order | 400 | S2 400 'Cannot modify order in complete status' |
| modifyorder | error_missing_price | 400 | S2 stringified dict, WITH mode key (unlike placeorder) |
| modifyorder | error_negative_price | 400 | S2  |
| modifyorder | error_unknown_orderid | 404 | S2 404 'Order <id> not found' |
| modifyorder | open_limit_price_and_qty | 200 | S2 {status, message:'Order modified successfully', orderid, mode} |
| modifyorder | sl_order_trigger | 200 | S2  |
| multioptiongreeks | nifty_ce_and_pe | 200 | data:[per-symbol results], summary:{total,success,failed}; status 'success'/'partial'/'error' with HTTP 200 even when all fail |
| multioptiongreeks | one_valid_one_unknown | 200 | status:'partial', HTTP 200 |
| multiquotes | empty_symbols_list | 400 | 400 'Shorter than minimum length 1.' |
| multiquotes | mixed_seven_symbols | 200 | NOTE key is `results` (not `data`): [{symbol, exchange, data:{quote fields}}]; order of results may differ from request order |
| multiquotes | with_one_unknown_symbol | 200 | 200 + status:success overall; failed item is {symbol, exchange, error:<string>} with no data key; unknown symbol listed first (order not preserved) |
| openposition | crudeoil_future_nrml | 200 | S2  |
| openposition | from_positionbook_or_default | 200 | no position -> {status:'success', quantity:0, mode}; quantity is an int (symbol=RELIANCE exchange=NSE product=MIS) |
| openposition | infy_after_smart_open | 200 | S2  |
| openposition | infy_after_smart_short | 200 | S2  |
| openposition | nifty_future_nrml | 200 | S2  |
| openposition | no_position_session2 | 200 | S2  |
| openposition | no_position_symbol | 200 |  |
| openposition | reliance_mis_long | 200 | S2  |
| openposition | sbin_mis_short | 200 | S2 negative int quantity for short |
| optionchain | bad_expiry | 404 | 404 |
| optionchain | banknifty_monthly_3_strikes | 200 |  |
| optionchain | nifty_nearest_expiry_3_strikes_with_greeks | 200 | adds implied_volatility/delta/gamma/theta/vega per leg, forward_price (expiry from /expiry is DD-MMM-YY; converted to DDMMMYY) |
| optionchain | nifty_nearest_expiry_5_strikes | 200 | flat response with chain:[{strike, ce:{...}, pe:{...}}], expiry_date echoed as DDMMMYY, expiry_ts/server_ts epoch seconds, labels ATM/ITMn/OTMn (expiry from /expiry is DD-MMM-YY; converted to DDMMMYY) |
| optiongreeks | nifty_option_default | 200 | flat response; expiry_date here is 'DD-Mon-YYYY' (06-Oct-2026) -- third date format; greeks nested |
| optiongreeks | nifty_option_with_rate_and_underlying | 200 |  |
| optiongreeks | unknown_symbol | 400 | 400 (first-pass run with wrong symbol format) |
| optionsmultiorder | bull_call_spread_2_legs | 200 | S2 results:[{action, exchange, leg, mode, offset, option_type, orderid, product, status, strike:null, symbol}], underlying, underlying_ltp |
| optionsmultiorder | error_empty_legs | 400 | S2 DIFFERENT error shape: {status:'error', message:'Validation error', errors:{legs:[...]}} |
| optionsmultiorder | one_leg_bad_offset | 200 | S2 200 success overall; failed leg has message and no orderid |
| optionsorder | atm_ce_buy | 200 | S2 flat: {exchange, offset, option_type, orderid, symbol, underlying, underlying_ltp, mode} |
| optionsorder | atm_ce_buy_with_splitsize | 200 | S2 with splitsize: results[] + split_size + total_quantity instead of orderid |
| optionsorder | error_bad_expiry | 404 | S2 404 'No strikes found for NIFTY expiring 01JAN20...' |
| optionsorder | error_bad_offset | 400 | S2 400 string, NO mode key |
| optionsorder | error_qty_not_lot_multiple | 400 | S2  |
| optionsorder | itm2_pe_buy | 200 | S2  |
| optionsorder | otm3_ce_sell | 200 | S2  |
| optionsymbol | expiry_with_dashes_as_returned_by_expiry_api | 404 | raw /expiry value (06-OCT-26) rejected with 404 'No strikes found' (passing the raw DD-MMM-YY value from /expiry is rejected) |
| optionsymbol | invalid_offset | 400 | 400 marshmallow message |
| optionsymbol | nifty_atm_ce | 200 | flat response (no data wrapper): {status, symbol, exchange, lotsize, tick_size, freeze_qty, underlying_ltp}; expiry_date must be DDMMMYY (expiry from /expiry is DD-MMM-YY; converted to DDMMMYY) |
| optionsymbol | nifty_atm_ce_session2 | 200 | S2  |
| optionsymbol | nifty_itm1_ce_with_strike_int | 200 | expiry from /expiry is DD-MMM-YY; converted to DDMMMYY |
| optionsymbol | nifty_otm2_pe | 200 | expiry from /expiry is DD-MMM-YY; converted to DDMMMYY |
| optionsymbol | underlying_is_future_symbol | 200 | underlying=NIFTY27OCT26FUT on NFO works, expiry inferred from future (underlying as NIFTYddMMMyyFUT on NFO; expiry inferred) |
| orderbook | after_cleanup | 200 | S2  |
| orderbook | default | 200 | data:{orders:[], statistics:{...}} + top-level mode:'analyze' |
| orderbook | populated | 200 | S2 orders newest first; statistics {total_buy_orders,total_completed_orders,total_open_orders,total_rejected_orders,total_sell_orders,total_trigger_pending_orders} |
| orderstatus | after_cancel | 200 | S2 order_status 'cancelled'; pending_quantity NOT zeroed |
| orderstatus | after_modify | 200 | S2  |
| orderstatus | limit_buy_cnc_sbin_far_below | 200 | S2  |
| orderstatus | market_buy_mis_reliance | 200 | S2 data uses key price_type (orderbook uses pricetype); no rejection_reason; timestamp 'YYYY-MM-DD HH:MM:SS' IST |
| orderstatus | market_buy_nrml_nifty_option | 200 | S2  |
| orderstatus | sl_buy_mis_reliance | 200 | S2  |
| orderstatus | slm_sell_mis_sbin | 200 | S2  |
| orderstatus | unknown_orderid | 404 | 404 {status:'error', message:'Order <id> not found', mode} (orderbook was empty -> unknown orderid error case) |
| orderstatus | unknown_orderid_session2 | 404 | S2  |
| ping | apikey_in_body | 200 | envelope {status, data:{broker, message:'pong'}} |
| ping | apikey_in_header_and_body | 200 | header ignored, body key used |
| ping | apikey_in_x_api_key_header | 400 | X-API-KEY header NOT honoured by /api/v1 endpoints -> 400 'Missing data for required field' (only telegram/whatsapp read the header) (Header-only auth: server reads apikey from JSON body only (schema required=True)) |
| ping | extra_unknown_field | 400 | schemas are strict: unknown field -> 400 {foo:['Unknown field.']} |
| placegttorder | error_bad_trigger_type | 400 | S2  |
| placegttorder | error_mis_product | 400 | S2 GTT only CNC/NRML |
| placegttorder | error_oco_missing_legs | 400 | S2  |
| placegttorder | error_unknown_symbol | 400 | S2 'Symbol not found' (differs from placeorder text) |
| placegttorder | oco_sell_cnc | 200 | S2  |
| placegttorder | single_buy_cnc_trigger_below | 200 | S2 {status, trigger_id:'GTT-YYMMDD-<8 hex>', mode} (SINGLE: one trigger) |
| placeorder | error_bad_action | 400 | S2  |
| placeorder | error_bad_exchange | 400 | S2  |
| placeorder | error_bad_pricetype | 400 | S2  |
| placeorder | error_bad_product | 400 | S2  |
| placeorder | error_invalid_apikey | 403 | S2 403 'Invalid openalgo apikey' |
| placeorder | error_limit_price_zero | 400 | S2 business rule (string): 'LIMIT orders require price' (LIMIT with price 0) |
| placeorder | error_missing_strategy | 400 | S2  |
| placeorder | error_missing_symbol | 400 | S2 stringified-dict message; NO mode key on placeorder schema errors |
| placeorder | error_negative_price | 400 | S2  |
| placeorder | error_option_qty_not_lot_multiple | 400 | S2 'Quantity must be in multiples of lot size 65' |
| placeorder | error_quantity_fractional_nse | 400 | S2  |
| placeorder | error_quantity_negative | 400 | S2  |
| placeorder | error_quantity_zero | 400 | S2 400; schema errors on order endpoints are a STRINGIFIED python dict: "{'quantity': ['Quantity must be a positive number.']}" (not an object as on read endpoints) |
| placeorder | error_sl_without_trigger | 400 | S2 'SL orders require trigger_price' (SL with trigger_price 0) |
| placeorder | error_unknown_symbol | 400 | S2 400 STRING message 'Symbol X not found on NSE' + mode |
| placeorder | limit_buy_cnc_sbin_far_below | 200 | S2 stays order_status 'open' (LIMIT far below LTP -> stays open) |
| placeorder | limit_buy_mis_reliance_far_below | 200 | S2  |
| placeorder | limit_sell_mis_reliance_far_above | 200 | S2  |
| placeorder | lowercase_action_buy | 200 | S2 lowercase action accepted |
| placeorder | market_buy_cnc_sbin | 200 | S2  |
| placeorder | market_buy_mis_nifty_option | 200 | S2  |
| placeorder | market_buy_mis_reliance | 200 | S2 {status:'success', orderid:'<14-digit string>', mode:'analyze'}; MARKET fills immediately at LTP (bid/ask 0 off-hours) |
| placeorder | market_buy_nrml_crudeoil_future_mcx | 200 | S2  |
| placeorder | market_buy_nrml_nifty_future | 200 | S2  |
| placeorder | market_buy_nrml_nifty_option | 200 | S2  |
| placeorder | market_sell_mis_sbin | 200 | S2  |
| placeorder | sl_buy_mis_reliance | 200 | S2 status 'trigger pending' (with a SPACE) (SL BUY: trigger above LTP) |
| placeorder | slm_sell_mis_sbin | 200 | S2 SL-M SELL: trigger below LTP |
| placesmartorder | error_missing_position_size | 400 | S2  |
| placesmartorder | error_unknown_symbol | 400 | S2  |
| placesmartorder | flat_to_short_minus_3 | 200 | S2 0 -> -3 |
| placesmartorder | no_action_already_at_5 | 200 | S2 quantity 0 + position matches -> 200 'No OpenPosition Found. Not placing Exit order.' (misleading text) (position already matches) |
| placesmartorder | no_action_qty_nonzero_position_matches | 200 | S2 200 'Positions Already Matched. No Action needed.' (no orderid) (position already -3, asked -3 with qty 3) |
| placesmartorder | open_from_flat_to_10 | 200 | S2 same shape as placeorder (flat -> +10 (BUY 10)) |
| placesmartorder | raise_10_to_15 | 200 | S2 +10 -> +15 |
| placesmartorder | reduce_15_to_5 | 200 | S2 +15 -> +5 |
| placesmartorder | to_zero | 200 | S2 +5 -> 0 |
| pnl | symbols_after_cleanup | 200 | S2  |
| pnl | symbols_in_live_mode | 200 | 200 because instance is in analyze mode; in live mode this returns 400 'only available in sandbox/analyzer mode' (only available in analyzer mode) |
| pnl | symbols_populated | 200 | S2 per-symbol rows (no ltp/avg) + top-level totals |
| portfolio | benchmarks | 200 | GET; data:[{exchange,name,symbol}] list of index benchmarks |
| portfolio | benchmarks_invalid_apikey | 403 | 403 |
| positionbook | after_closeposition | 200 | S2 closed (qty 0) positions are NOT listed |
| positionbook | default | 200 | data:[] plus top-level total_pnl/total_pnl_today/total_today_realized_pnl/total_unrealized_pnl (analyze mode only) |
| positionbook | final | 200 | S2  |
| positionbook | populated | 200 | S2 rows {average_price, exchange, lot_size (float), ltp, pnl, pnlpercent, product, quantity, symbol, today_realized_pnl, total_pnl_today, unrealized_pnl} |
| quotes | banknifty_nse_index | 200 |  |
| quotes | crudeoil_future_mcx | 200 | symbol=CRUDEOIL19OCT26FUT |
| quotes | nifty_future_nfo | 200 | symbol=NIFTY27OCT26FUT |
| quotes | nifty_nse_index | 200 |  |
| quotes | nifty_option_nfo | 200 | symbol=NIFTY06OCT2622400CE |
| quotes | reliance_bse | 200 |  |
| quotes | reliance_nse | 200 | data:{ask,ask_qty,bid,bid_qty,high,low,ltp,oi,open,prev_close,volume}; market closed -> bid/ask/volume 0; ints and floats mixed (prev_close 1187 vs ltp 1167.7) |
| quotes | reliance_nse_header_and_body | 200 |  |
| quotes | reliance_nse_x_api_key_header | 400 | 400, header not supported |
| quotes | reliance_session2 | 200 | S2  |
| quotes | sbin_nse | 200 |  |
| quotes | sbin_session2 | 200 | S2  |
| quotes | symbol_with_dashed_expiry | 400 | 400 'Symbol ... not found for exchange' (same shape as unknown symbol) (symbol built with dashed expiry is not found) |
| search | nifty_nfo | 200 |  |
| search | nifty_no_exchange | 200 |  |
| search | no_results | 200 | 200 {status:success, data:[], message:'No matching symbols found'} |
| search | reliance_nse | 200 | data: list of symbol rows (same row shape as /symbol minus id); truncated to 25 |
| splitorder | error_missing_splitsize | 400 | S2  |
| splitorder | error_splitsize_zero | 400 | S2  |
| splitorder | sbin_10_split_3 | 200 | S2 results:[{order_num, orderid, quantity, status}] remainder as last chunk; split_size, total_quantity |
| strategy | list | 200 | POST /strategy/list; data:[] |
| strategy | status_unknown_id | 400 | strategy_id must be an INTEGER ('Not a valid integer.') |
| symbol | crudeoil_future_mcx | 200 |  |
| symbol | crudeoil_future_session2 | 200 | S2  |
| symbol | nifty_future_nfo | 200 |  |
| symbol | nifty_future_session2 | 200 | S2  |
| symbol | nifty_nse_index | 200 |  |
| symbol | nifty_option_nfo | 200 | brsymbol uses Zerodha weekly code NIFTY26O0622400CE; expiry 'DD-MMM-YY' |
| symbol | reliance_nse | 200 | data:{brexchange,brsymbol,exchange,expiry,freeze_qty,id,instrumenttype,lotsize,name,strike,symbol,tick_size,token}; token is 'a::::b' composite string |
| symbol | unknown_symbol | 404 | 404 |
| syntheticfuture | nifty_monthly_expiry | 200 |  |
| syntheticfuture | nifty_nearest_expiry | 200 | flat: {atm_strike, expiry (DDMMMYY), synthetic_future_price, underlying, underlying_ltp} (expiry from /expiry is DD-MMM-YY; converted to DDMMMYY) |
| ticker | invalid_apikey_json | 403 | 403 standard error |
| ticker | invalid_apikey_txt | 500 | BUG: 500 Internal Server Error (TextResponse tuple return breaks flask-restx) |
| ticker | missing_from_to | 400 | 400 'Field may not be null.' for start_date/end_date (internal names leak) |
| ticker | no_exchange_prefix_defaults | 200 | BUG-LIKE: path without 'EXCH:' prefix silently falls back to NSE:RELIANCE (ticker.py) (no 'EXCH:' prefix -> server silently uses NSE:RELIANCE (see ticker.py)) |
| ticker | nse_index_nifty_D_json | 200 |  |
| ticker | nse_reliance_5m_json | 200 |  |
| ticker | nse_reliance_5m_txt | 200 | intraday txt adds HH:MM:SS column: 'NSE:RELIANCE,YYYY-MM-DD,HH:MM:SS,o,h,l,c,v' |
| ticker | nse_reliance_D_json | 200 | GET /ticker/EXCH:SYMBOL?apikey&interval&from&to; same JSON shape as /history |
| ticker | nse_reliance_D_txt | 200 | format=txt: text/plain CSV lines 'NSE:RELIANCE,YYYY-MM-DD,o,h,l,c,v' (no header) |
| tradebook | after_cleanup | 200 | S2  |
| tradebook | default | 200 | data:[] + mode |
| tradebook | populated | 200 | S2 rows {action, average_price, exchange, orderid, price, product, quantity, strategy, symbol, timestamp, trade_value, tradeid:'TRADE-YYYYMMDD-HHMMSS-<8hex>'} |

## WebSocket fixtures (`websocket/*.jsonl`)

One JSON object per line: `{ts, iso, direction: send|recv|note, message|raw, ...}`. market_data lines capped at 30 per listen window; totals in the `note` lines.

| File | Covers | Note |
|---|---|---|
| 01_connect_auth_subscribe_flow.jsonl | connect, authenticate, ping, get_broker_info, get_supported_brokers, subscribe LTP(1)/Quote/Depth(3) for RELIANCE NSE + NIFTY NSE_INDEX (20s each), depth 20 via `depth` and legacy `depth_level`, depth 50, single-symbol form, invalid mode, no symbols, unknown symbol, bad exchange, unsubscribe, unsubscribe not-subscribed, mixed per-symbol modes, subscribe_orders/unsubscribe_orders, unsubscribe_all, invalid action, `type` alias, malformed JSON, JSON array, empty object | Ticks DID arrive on Saturday (12 total): one snapshot per symbol shortly after each subscribe (stale last-traded values, volume 0). Depth 20/50 acks report depth 20/50 but no depth ticks arrived. Server answers higher-mode subscription by also emitting lower-mode copies (mode 1 message after mode 2 subscribe). |
| 02_subscribe_before_auth.jsonl | subscribe / unsubscribe_all / subscribe_orders / get_broker_info before authenticate; ping before auth | All gated actions -> {status:'error', code:'NOT_AUTHENTICATED'} (request_id echoed when provided). `ping` works WITHOUT auth. |
| 03_invalid_apikey_auth.jsonl | authenticate with bad key, with no key, `auth`+`apikey` aliases | {status:'error', code:'AUTHENTICATION_ERROR', message:'Invalid API key' | 'API key is required'}; connection stays open (pong works afterwards). |
| 04_auth_grace_timeout.jsonl | connect and send nothing | Server closes after ~15s with close code 4401 reason 'auth timeout' (WS_AUTH_GRACE_SECONDS). |
| 05_auth_alias_forms_and_mode_labels.jsonl | `type`:'auth' + `apikey` alias, re-auth on same socket, mode labels ltp/QUOTE/depth, mode '2' (string digit), mode 2.0 (float) | Labels are case-insensitive and canonicalised to LTP/Quote/Depth in the ack. String digit '2' is REJECTED (INVALID_MODE); float rejected 'Mode must be int or str, got float'. |

## Observed conventions

### Transport and auth
- All `/api/v1/*` data endpoints are `POST` with `Content-Type: application/json` and `apikey` **inside the JSON body**. Exceptions that are `GET` with `?apikey=` in the query string: `/instruments`, `/ticker/<EXCH:SYMBOL>`, `/chart`, `/portfolio/benchmarks`.
- `X-API-KEY` header is **not** honoured by any `/api/v1` endpoint captured (only `telegram/*` and `whatsapp/*` read it). Header-only auth -> `400 {"status":"error","message":{"apikey":["Missing data for required field."]}}`.
- Marshmallow schemas are strict: unknown JSON fields -> 400 `{"<field>":["Unknown field."]}`; `exchange` is case-sensitive and must be one of `NSE, NFO, CDS, BSE, BFO, BCD, MCX, NCDEX, NCO, NSE_INDEX, BSE_INDEX, MCX_INDEX, GLOBAL_INDEX, CRYPTO`.
- Trailing slash tolerated (`/ping/` == `/ping`).

### Success envelope
- Most endpoints: `{"status":"success","data":...}`. Account endpoints in analyze mode add top-level `"mode":"analyze"`; `positionbook`/`pnl/symbols` add top-level `total_pnl`, `total_pnl_today`, `total_today_realized_pnl`, `total_unrealized_pnl`.
- **Flat (no `data` wrapper)** responses: `optionsymbol`, `optionchain`, `optiongreeks`, `syntheticfuture`, `openposition` (`{"status","quantity","mode"}`), `multioptiongreeks` (`data` list + `summary`), `expiry` (`data` list + `message`).
- Numbers are JSON numbers (never strings) in all market-data and sandbox account responses; ints and floats are mixed within the same object (`prev_close: 1187`, `ltp: 1167.7`), so parse as f64.
- `token` in symbol rows is a composite string `"<instrument_token>::::<exchange_token>"`.

### Error envelope
- Application errors: `{"status":"error","message": <string | object>}`. `message` is a **string** for business errors (invalid key, symbol not found) and an **object `{field:[msgs]}`** for schema validation errors.
- HTTP codes: 400 validation / unknown symbol / bad exchange; 403 invalid apikey (`"Invalid openalgo apikey"`); 404 symbol lookup miss, unknown orderid, unknown route, **and wrong HTTP method** (flask-restx returns `{"status":"error","message":"Not found","path":...}` for GET on a POST route, not 405); 500 for unhandled cases.
- Framework-level errors do NOT carry `status`: malformed JSON / empty body -> 400 `{"message":"The browser (or proxy) sent a request that this server could not understand."}`; JSON array body -> 500 `{"message":"Internal Server Error"}`. A client must tolerate a missing `status`.
- `ticker` with `format=txt` + invalid apikey -> 500 (server bug); JSON form -> 403.

### Headers
- Response headers: `Content-Type: application/json` (`text/plain` for ticker txt, `text/csv` + `Content-Disposition: attachment` for instruments csv), `Access-Control-Allow-Origin: http://127.0.0.1:5000`, strict CSP/X-Frame-Options/X-Content-Type-Options security headers. **No `X-RateLimit-*` or `Retry-After` headers** on normal responses.

### Rate limiting
- `API_RATE_LIMIT` on this instance is `100 per second` (env; code default 10/s for ping, 50/s elsewhere), flask-limiter moving-window, in-memory, keyed by remote address. Order endpoints use `ORDER_RATE_LIMIT=10 per second`. A burst of 103 pings completed in 2.6 s with zero 429s (server throughput < limit), so the 429 body was not observable; per flask-limiter defaults it would be `429 {"message": "..."}`.

### Date / time formats (four different ones)
- Request dates: `start_date`/`end_date`/`from`/`to`/`market/timings.date` = `YYYY-MM-DD`. `expiry_date` for optionsymbol/optionchain/syntheticfuture = `DDMMMYY` (`06OCT26`).
- `/expiry` returns `DD-MMM-YY` (`06-OCT-26`); `/symbol` rows return `expiry` as `DD-MMM-YY`; `/optiongreeks` returns `expiry_date` as `DD-Mon-YYYY` (`06-Oct-2026`); `/optionchain` echoes `DDMMMYY`. Clients must convert `DD-MMM-YY` -> `DDMMMYY` to build symbols (`NIFTY27OCT26FUT`, `NIFTY06OCT2622400CE`).
- History/ticker candle `timestamp` = epoch **seconds** (int). market/holidays & market/timings `start_time`/`end_time` = epoch **milliseconds**. optionchain `expiry_ts`/`server_ts` = epoch seconds. WebSocket `timestamp`/`ltt`/`server_timestamp` = epoch milliseconds.
- Ticker `format=txt`: `EXCH:SYMBOL,YYYY-MM-DD,o,h,l,c,v` for `D`; intraday adds `HH:MM:SS` as third column; no header line.

### Symbol conventions
- Equity `RELIANCE`/`NSE`; index `NIFTY`/`NSE_INDEX`; futures `NIFTY27OCT26FUT`/`NFO`, `CRUDEOIL19OCT26FUT`/`MCX`; options `NIFTY06OCT2622400CE`/`NFO`. NIFTY lot size 65, freeze 1800.

### WebSocket protocol (ws://127.0.0.1:8765, `websockets/15.0.1`)
- Client -> server JSON with `action` (alias `type`): `authenticate|auth` (`api_key` or `apikey`), `subscribe`, `unsubscribe`, `unsubscribe_all`, `subscribe_orders`, `unsubscribe_orders`, `get_broker_info`, `get_supported_brokers`, `ping`. Optional `request_id` is echoed on acks and on errors.
- Auth ack: `{"type":"auth","status":"success","message","broker","user_id","supported_features":{"ltp","quote","depth"}}`. Unauthenticated sockets are closed after 15 s with code **4401 "auth timeout"**; a failed auth does NOT close the socket. `ping` works before auth; everything else -> `NOT_AUTHENTICATED`.
- Subscribe: `{"action":"subscribe","symbols":[{"symbol","exchange"}],"mode":1|2|3|"LTP"|"Quote"|"Depth" (case-insensitive),"depth":5|20|50 (legacy key `depth_level` also accepted),"request_id"}`; single-symbol form `{"symbol","exchange"}` at top level also works. Mode as string digit `"2"` or float is rejected (`INVALID_MODE`).
- Subscribe ack: `{"type":"subscribe","status":"success|partial","subscriptions":[{"symbol","exchange","status","mode":"LTP|Quote|Depth","depth":N,"broker"} | {...,"status":"error","message"}],"message":"Subscription processing complete","broker","request_id"}`. Unknown symbol/exchange -> `status:"partial"` with per-item `"Token not found for X on Y"`; HTTP-level success.
- Unsubscribe ack: `{"type":"unsubscribe","status":"success|partial","message","successful":[{symbol,exchange,mode,status,broker}],"failed":[{...,"mode":null,"message"}],"broker","request_id"}`. `unsubscribe_all` uses the same `type:"unsubscribe"` ack listing every active key. Unsubscribing a never-subscribed key still reports success.
- Errors: `{"status":"error","code":"NOT_AUTHENTICATED|AUTHENTICATION_ERROR|INVALID_MODE|INVALID_PARAMETERS|INVALID_ACTION|INVALID_JSON|SERVER_ERROR|BROKER_ERROR|PROCESSING_ERROR","message", "request_id"?}` with no `type` field. `{}` -> `INVALID_ACTION "Invalid action: None"`; a JSON array -> `SERVER_ERROR "'list' object has no attribute 'get'"`.
- Market data: `{"type":"market_data","symbol","exchange","mode":1|2|3 (int),"broker","data":{...}}`. `data` repeats `symbol`, `exchange` and a lowercase `mode` label (`"ltp"|"quote"|"depth"`), plus `ltp`, `ltt` (ms), `timestamp` (ms); quote adds `volume,last_quantity,average_price,total_buy_quantity,total_sell_quantity,open,high,low,close` (index quote instead has `price_change`, `price_change_percent` and lacks OHLC when closed). A client subscribed at mode 2 also receives mode-1 copies when a mode-1 subscription exists on the same socket. Field names differ from the docs (`ltt` vs documented none; docs' `change`/`change_percent` appear as `price_change`/`price_change_percent`).
- Order stream: `subscribe_orders` -> `{"type":"subscribe_orders","status":"success","message":"Subscribed to order updates"}`; `unsubscribe_orders` mirrors it. No `order_update` events observed (WS capture predates session 2 orders).
- Misc acks: `pong` `{"type":"pong","status":"success","server_timestamp": ms}`; `broker_info` `{"type","status","broker","adapter_status":"connected","user_id"}`; `supported_brokers` `{"type","status","brokers":[36 names],"count":36}`.

### Not captured (deliberately) and read-only notes for the desktop clone
- Session 1 called no mutating endpoint; session 2 exercised them all in analyze mode (see section below). Request contracts (from `restx_api/schemas.py`, `*` = required): see `MUTATING_SCHEMAS.md`.
- `/sandbox/*` web routes (`/sandbox/`, `/sandbox/api/configs`, `/sandbox/update` POST, `/sandbox/reset` POST, `/sandbox/reload-squareoff` POST, `/sandbox/squareoff-status`, `/sandbox/mypnl`, `/sandbox/mypnl/api/data`, `/sandbox/mypnl/export/{daily,positions,holdings,trades}`) are all guarded by `@check_session_validity` (browser session cookie), not by apikey, so they are not reachable from the API-key client and were not called.


### Order-mutating endpoints (session 2, analyze mode only)
- **Mode field**: every order/account response in analyze mode carries top-level `"mode":"analyze"`. Exceptions: placeorder schema errors, optionsorder errors, optionsmultiorder validation errors and analyzer toggle errors have no `mode`. Live mode would omit it (toggle response reports `mode:"live"`).
- **Order id**: 14-digit numeric STRING `YYMMDD` + 8 digits (e.g. `26100376733573`). GTT id: `GTT-YYMMDD-<8 hex>`. Trade id: `TRADE-YYYYMMDD-HHMMSS-<8 hex>`.
- **Status strings** (`order_status`): `complete`, `open`, `trigger pending` (with a space), `cancelled`; statistics also count `rejected`. GTT status: `active`, `cancelled`; GTT `trigger_type` stored as `single` / `two-leg` (request uses `SINGLE` / `OCO`).
- **Price-type key differs**: orderbook rows use `pricetype`, orderstatus `data` uses `price_type`.
- **Timestamps**: order/trade `timestamp` = `YYYY-MM-DD HH:MM:SS` IST without zone; GTT `created_at`/`updated_at`/`expires_at` = ISO-8601 with microseconds, IST, no zone; funds `last_reset` = `YYYY-MM-DD HH:MM:SS`.
- **Error message shape on order endpoints**: marshmallow failures are a STRINGIFIED Python dict (`"{'quantity': ['Quantity must be a positive number.']}"`, nested `"{'orders': {0: {...}}}"` for basket) - unlike read endpoints (incl. gttorderbook) where `message` is a JSON object. optionsmultiorder uses yet another shape: `{status:'error', message:'Validation error', errors:{field:[...]}}`. Business errors are plain strings. A Rust client should accept `message` as `String | Object` and an optional `errors`.
- **HTTP codes**: 400 validation/business rule/unknown symbol; 403 bad apikey; 404 unknown orderid (`Order <id> not found`), unknown/inactive GTT, optionsorder bad expiry; 429 order rate limit (`{"message":"10 per 1 second"}`, no `status`, no Retry-After); 500 on two server bugs below.
- **Batch shapes**: basketorder/splitorder/optionsmultiorder return HTTP 200 + `status:"success"` even when some legs fail; per-leg `status` decides. optionsorder with `splitsize` swaps `orderid` for `results[]` + `split_size` + `total_quantity`.
- **Success-without-action**: cancelallorder with nothing open, closeposition with no positions, and placesmartorder no-op cases all return 200 `status:"success"` with only a `message`.
- **Sandbox behaviour**: MARKET orders fill instantly at LTP even on a Saturday (bid/ask 0); MIS is blocked only between square-off time (15:15 NSE) and 09:00 IST; quantity must be a lot multiple for F&O; same-day CNC buys do not appear in holdings; closed positions disappear from positionbook.
- **Server bugs found** (web app, analyze mode): (1) `cancelallorder` filters on `"trigger_pending"` but stored status is `"trigger pending"`, so SL/SL-M trigger-pending orders are NOT cancelled (`services/sandbox_service.py` sandbox_cancel_all_orders). (2) Any failed sandbox `modifygttorder` returns 500 because `GTTModifyFailedEvent` (events/order_events.py) has no `exchange` field but `services/modify_gtt_order_service.py` passes `exchange=`. (3) After an OCO GTT modify raised `margin_blocked` (1059.1 -> 1106.8) and a later closeposition, `cancelgttorder` returns 500 "Could not release 1106.80 margin ... more than the reserved margin"; the GTT `GTT-261003-56ad1482` stays `active` in the sandbox and could not be cleaned up via API.


### Web broker changes deliberately not copied
- **Angel daily candles (web 4c8ee632c, #2176, 2026-10-06)**: the web dropped the +05:30 shift on Angel `D` candles, so they are now stamped at IST midnight (18:30 UTC of the previous day). Every other web broker, and `history/nifty_index_D.json` above (`1788220800` = 2026-09-01 00:00 UTC), stamps a daily candle at 00:00 UTC of its date. The desktop keeps 00:00 UTC for Angel (`brokers/angel/data.rs` `parse_candles`). The IST request window from the same commit is ported.
