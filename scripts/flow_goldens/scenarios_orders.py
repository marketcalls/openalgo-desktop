"""Order nodes (lane nodes-orders)."""

from lib import (
    BASKET, BROKER_REJECT, CANCEL, CANCEL_ALL, CLOSE, DESC_CONTRACT, EXPIRY, MODIFY, MULTI_ORDER,
    NIFTY_CONTRACT, OPTIONS_ORDER, ORDER, ORDER_ANALYZE, ORDER_STATUS, SESSION_EXPIRED, SPLIT, err,
    node, ok, reply,
)

F = "orders"
WH = {"webhook": {"symbol": "sbin", "action": "SELL", "qty": "15", "price": "801.5", "exchange": "NSE"}}

# --- placeOrder -------------------------------------------------------------------

PO = {"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 10, "priceType": "MARKET",
      "price": 0, "triggerPrice": 0, "outputVariable": "orderResult"}
node(F, "placeOrder", "happy_market", PO, script=[reply("placeorder", ORDER)],
     parity=["N-placeOrder-01", "N-placeOrder-03", "N-common-09", "C-08", "CL-04"])
node(F, "placeOrder", "lowercase_symbol_upper_cased", {**PO, "symbol": "sbin"},
     script=[reply("placeorder", ORDER)], parity=["N-common-17"])
node(F, "placeOrder", "analyzer_reply", PO, script=[reply("placeorder", ORDER_ANALYZE)], analyzer=True,
     parity=["N-placeOrder-03"], note="Analyzer mode is decided by the service; the node stores the reply as is.")
node(F, "placeOrder", "strategy_tag", {**PO, "strategyTag": "gap-fade"}, script=[reply("placeorder", ORDER)],
     parity=["N-common-07", "CL-03"])
node(F, "placeOrder", "workflow_name_is_the_tag", {k: v for k, v in PO.items() if k != "outputVariable"},
     workflow_name="Opening Range", script=[reply("placeorder", ORDER)], parity=["N-common-07"])
node(F, "placeOrder", "limit_order", {**PO, "priceType": "LIMIT", "price": 800.5},
     script=[reply("placeorder", ORDER)], parity=["N-common-17"])
node(F, "placeOrder", "sl_order", {**PO, "priceType": "SL", "price": 801, "triggerPrice": 800},
     script=[reply("placeorder", ORDER)], parity=["N-common-17"])
node(F, "placeOrder", "slm_order", {**PO, "priceType": "SL-M", "triggerPrice": "800"},
     script=[reply("placeorder", ORDER)], parity=["N-common-17"])
node(F, "placeOrder", "broker_rejects", PO, script=[reply("placeorder", BROKER_REJECT)],
     parity=["N-placeOrder-03", "N-common-16", "CL-02"])
node(F, "placeOrder", "interpolated_fields",
     {"symbol": "{{webhook.symbol}}", "exchange": "{{webhook.exchange}}", "action": "{{webhook.action}}",
      "quantity": "{{webhook.qty}}", "priceType": "LIMIT", "price": "{{webhook.price}}",
      "outputVariable": "orderResult"},
     vars_in=WH, script=[reply("placeorder", ORDER)], parity=["N-common-13", "N-common-17"])
node(F, "placeOrder", "quantity_from_number_variable", {**PO, "quantity": "{{sizing.qty}}"},
     vars_in={"sizing": {"qty": 25}}, script=[reply("placeorder", ORDER)], parity=["N-common-13"])
node(F, "placeOrder", "quantity_fraction_truncates", {**PO, "quantity": 2.9}, script=[reply("placeorder", ORDER)],
     parity=["N-common-14"])
node(F, "placeOrder", "unresolved_quantity", {**PO, "quantity": "{{webhook.qty}}"},
     parity=["N-common-03", "N-common-04", "N-common-05"])
node(F, "placeOrder", "unresolved_two_fields", {**PO, "symbol": "{{signal.symbol}}", "quantity": "{{webhook.qty}}"},
     parity=["N-common-03", "N-common-04"])
node(F, "placeOrder", "unresolved_label_field_allowed", {**PO, "strategyTag": "{{nope}}"},
     script=[reply("placeorder", ORDER)], parity=["N-common-02"])
node(F, "placeOrder", "missing_symbol", {k: v for k, v in PO.items() if k != "symbol"},
     parity=["N-placeOrder-02", "N-common-13", "N-common-16"])
node(F, "placeOrder", "blank_symbol", {**PO, "symbol": "   "}, parity=["N-common-14"])
node(F, "placeOrder", "symbol_not_text", {**PO, "symbol": 500112}, parity=["N-common-14"])
node(F, "placeOrder", "symbol_resolves_to_list", {**PO, "symbol": "{{picks}}"}, vars_in={"picks": ["SBIN", "INFY"]},
     parity=["N-common-14"])
node(F, "placeOrder", "bad_exchange", {**PO, "exchange": "NASDAQ"}, parity=["N-common-14"])
node(F, "placeOrder", "bad_action", {**PO, "action": "HOLD"}, parity=["N-common-14"])
node(F, "placeOrder", "bad_price_type", {**PO, "priceType": "STOP"}, parity=["N-common-14"])
node(F, "placeOrder", "bad_product", {**PO, "product": "BO"}, parity=["N-common-14"])
node(F, "placeOrder", "quantity_zero", {**PO, "quantity": 0}, parity=["N-common-14", "N-common-17"])
node(F, "placeOrder", "quantity_text", {**PO, "quantity": "ten"}, parity=["N-common-14"])
node(F, "placeOrder", "quantity_boolean", {**PO, "quantity": True}, parity=["N-common-14"])
node(F, "placeOrder", "quantity_blank", {**PO, "quantity": ""}, parity=["N-common-14"])
node(F, "placeOrder", "price_not_finite", {**PO, "priceType": "LIMIT", "price": "inf"}, parity=["N-common-14"])
node(F, "placeOrder", "limit_without_price", {**PO, "priceType": "LIMIT", "price": 0}, parity=["N-common-18"])
node(F, "placeOrder", "sl_without_trigger", {**PO, "priceType": "SL", "price": 801, "triggerPrice": 0},
     parity=["N-common-18"])
node(F, "placeOrder", "slm_without_trigger", {**PO, "priceType": "SL-M"}, parity=["N-common-18"])
node(F, "placeOrder", "first_error_by_field_order", {**PO, "exchange": "XX", "action": "HOLD", "quantity": 0},
     parity=["N-common-17"], note="symbol, exchange, action, quantity, ... : the first bad field is reported.")
node(F, "placeOrder", "editor_defaults", {"symbol": "RELIANCE", "exchange": "NSE", "action": "BUY", "quantity": 1,
                                          "priceType": "MARKET", "price": 0, "triggerPrice": 0},
     script=[reply("placeorder", ORDER)], parity=["N-placeOrder-01"])
for exch in ("NSE", "BSE", "NFO", "BFO", "CDS", "BCD", "MCX", "NCDEX", "NCO"):
    node(F, "placeOrder", f"default_product_{exch.lower()}", {**PO, "exchange": exch, "symbol": "SYM"},
         script=[reply("placeorder", ORDER)], parity=["C-08", "N-common-17"])
node(F, "placeOrder", "stored_product_wins", {**PO, "exchange": "NFO", "product": "MIS"},
     script=[reply("placeorder", ORDER)], parity=["C-08"])

# --- smartOrder -------------------------------------------------------------------

SO = {"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 5, "positionSize": 5,
      "priceType": "MARKET", "outputVariable": "smart"}
node(F, "smartOrder", "happy", SO, script=[reply("placesmartorder", ORDER)],
     parity=["N-smartOrder-01", "N-smartOrder-02", "CL-05"])
node(F, "smartOrder", "quantity_zero_allowed", {**SO, "quantity": 0, "positionSize": 0},
     script=[reply("placesmartorder", ORDER)], parity=["N-smartOrder-01"])
node(F, "smartOrder", "short_target", {**SO, "action": "SELL", "positionSize": -10},
     script=[reply("placesmartorder", ORDER)], parity=["N-smartOrder-01"])
node(F, "smartOrder", "position_size_text", {**SO, "positionSize": "flat"}, parity=["N-smartOrder-02"])
node(F, "smartOrder", "quantity_negative", {**SO, "quantity": -1}, parity=["N-common-14"])
node(F, "smartOrder", "broker_rejects", SO, script=[reply("placesmartorder", BROKER_REJECT)], parity=["N-smartOrder-02"])
node(F, "smartOrder", "default_product_mcx", {**SO, "exchange": "MCX", "symbol": "CRUDEOIL17NOV26FUT"},
     script=[reply("placesmartorder", ORDER)], parity=["C-08"])
node(F, "smartOrder", "unresolved_position_size", {**SO, "positionSize": "{{target}}"}, parity=["N-common-03"])

# --- optionsOrder -----------------------------------------------------------------

OO = {"underlying": "NIFTY", "exchange": "NSE_INDEX", "expiryType": "current_week", "offset": "ATM",
      "optionType": "CE", "action": "BUY", "quantity": 2, "priceType": "MARKET", "product": "NRML",
      "price": 0, "triggerPrice": 0, "outputVariable": "opt"}
MC = {"master_contract": NIFTY_CONTRACT}
node(F, "optionsOrder", "happy_current_week", OO, env=MC,
     script=[reply("expiry", EXPIRY), reply("optionsorder", OPTIONS_ORDER)],
     parity=["N-optionsOrder-01", "N-optionsOrder-03", "N-optionsOrder-04", "N-optionsOrder-05",
             "N-optionsOrder-06", "N-common-22", "N-common-23", "C-11", "CL-17"])
for et in ("next_week", "current_month", "next_month"):
    node(F, "optionsOrder", f"expiry_type_{et}", {**OO, "expiryType": et}, env=MC,
         script=[reply("expiry", EXPIRY), reply("optionsorder", OPTIONS_ORDER)], parity=["C-11", "N-common-23"])
node(F, "optionsOrder", "month_end_rolls_to_next_month", {**OO, "expiryType": "next_month"}, env=MC,
     clock="2026-12-30T14:00:00",
     script=[reply("expiry", ok(data=["30-DEC-26", "06-JAN-27", "27-JAN-27", "24-FEB-27"])),
             reply("optionsorder", OPTIONS_ORDER)], parity=["C-11"])
node(F, "optionsOrder", "explicit_expiry_date", {**OO, "expiryDate": "25nov26"}, env=MC,
     script=[reply("optionsorder", OPTIONS_ORDER)], parity=["N-optionsOrder-02", "N-optionsOrder-05"])
node(F, "optionsOrder", "expiry_type_holds_a_date", {**OO, "expiryType": "25NOV26"}, env=MC,
     script=[reply("optionsorder", OPTIONS_ORDER)], parity=["N-optionsOrder-02"])
node(F, "optionsOrder", "bad_expiry_date", {**OO, "expiryDate": "2026-11-25"}, env=MC, parity=["N-optionsOrder-02"])
node(F, "optionsOrder", "bad_expiry_type", {**OO, "expiryType": "weekly"}, env=MC, parity=["N-optionsOrder-02", "C-06"])
node(F, "optionsOrder", "bad_offset", {**OO, "offset": "OTM51"}, env=MC, parity=["N-optionsOrder-01", "C-04"])
node(F, "optionsOrder", "offset_leading_zero", {**OO, "offset": "itm01"}, env=MC, parity=["C-04"])
node(F, "optionsOrder", "offset_lower_case_ok", {**OO, "offset": "otm2"}, env=MC,
     script=[reply("expiry", EXPIRY), reply("optionsorder", OPTIONS_ORDER)], parity=["C-04"])
node(F, "optionsOrder", "offset_unresolved", {**OO, "offset": "{{signal.offset}}"}, env=MC, parity=["N-common-13"])
node(F, "optionsOrder", "bad_option_type", {**OO, "optionType": "CALL"}, env=MC, parity=["N-optionsOrder-01"])
node(F, "optionsOrder", "split_size", {**OO, "splitSize": 50}, env=MC,
     script=[reply("expiry", EXPIRY), reply("optionsorder", OPTIONS_ORDER)], parity=["N-optionsOrder-04"])
node(F, "optionsOrder", "split_size_negative", {**OO, "splitSize": -1}, env=MC, parity=["N-common-14"])
node(F, "optionsOrder", "bad_exchange_present", {**OO, "exchange": "XYZ"}, env=MC, parity=["N-optionsOrder-01"])
node(F, "optionsOrder", "no_exchange_key", {k: v for k, v in OO.items() if k != "exchange"}, env=MC,
     script=[reply("expiry", EXPIRY), reply("optionsorder", OPTIONS_ORDER)], parity=["N-common-19"])
node(F, "optionsOrder", "lowercase_underlying", {**OO, "underlying": "nifty"}, env=MC,
     script=[reply("expiry", EXPIRY), reply("optionsorder", OPTIONS_ORDER)], parity=["N-optionsOrder-01"])
node(F, "optionsOrder", "sensex_routes_bfo", {**OO, "underlying": "SENSEX", "exchange": "NSE_INDEX"}, env=MC,
     script=[reply("expiry", ok(data=["30-OCT-26", "06-NOV-26", "27-NOV-26"])), reply("optionsorder", OPTIONS_ORDER)],
     parity=["N-common-19"])
node(F, "optionsOrder", "bse_declared", {**OO, "underlying": "BANKEX", "exchange": "BSE"}, env={"master_contract": []},
     script=[reply("expiry", ok(data=["30-OCT-26", "27-NOV-26"])), reply("optionsorder", OPTIONS_ORDER)],
     parity=["N-common-19", "N-common-22"])
node(F, "optionsOrder", "mcx_crudeoil", {**OO, "underlying": "CRUDEOIL", "exchange": "MCX", "quantity": 1}, env=MC,
     script=[reply("expiry", ok(data=["17-NOV-26", "16-DEC-26", "18-JAN-27"])), reply("optionsorder", OPTIONS_ORDER)],
     parity=["N-optionsOrder-07", "N-common-20", "N-common-19"])
node(F, "optionsOrder", "mcx_gold_declared_nfo", {**OO, "underlying": "GOLD", "exchange": "NFO", "quantity": 1}, env=MC,
     script=[reply("expiry", ok(data=["25-NOV-26", "28-JAN-27"])), reply("optionsorder", OPTIONS_ORDER)],
     parity=["N-common-19", "N-common-20"])
node(F, "optionsOrder", "next_week_single_expiry", {**OO, "expiryType": "next_week"}, env=MC,
     script=[reply("expiry", ok(data=["25-NOV-26"]))], parity=["N-optionsOrder-05", "N-common-23", "C-11"])
node(F, "optionsOrder", "expiry_fetch_fails", OO, env=MC, script=[reply("expiry", SESSION_EXPIRED)],
     parity=["N-common-23", "N-optionsOrder-05"])
node(F, "optionsOrder", "expiry_list_empty", OO, env=MC, script=[reply("expiry", ok(data=[]))], parity=["N-common-23"])
node(F, "optionsOrder", "expiry_list_unparseable", OO, env=MC, script=[reply("expiry", ok(data=["soon", "2026-11-25"]))],
     parity=["N-common-23", "C-09"])
node(F, "optionsOrder", "expiry_reply_not_a_list", OO, env=MC, script=[reply("expiry", ok(data=5))],
     parity=["N-common-23"])
node(F, "optionsOrder", "lot_size_not_in_master", {**OO, "underlying": "FINNIFTY"}, env=MC,
     parity=["N-optionsOrder-04", "N-common-22"])
node(F, "optionsOrder", "lot_size_fallback_table", OO, env={"master_contract": []},
     script=[reply("expiry", EXPIRY), reply("optionsorder", OPTIONS_ORDER)], parity=["N-common-22"])
node(F, "optionsOrder", "lot_size_no_fallback", {**OO, "underlying": "TATAMOTORS", "exchange": "NFO"},
     env={"master_contract": []}, parity=["N-common-22"])
node(F, "optionsOrder", "lot_size_unreadable", OO, env={"master_contract": "unreadable"}, parity=["N-common-22"])
node(F, "optionsOrder", "lot_size_from_symbol_prefix", OO, env={"master_contract": DESC_CONTRACT},
     script=[reply("expiry", EXPIRY), reply("optionsorder", OPTIONS_ORDER)], parity=["N-common-21", "N-common-22"])
node(F, "optionsOrder", "limit_without_price", {**OO, "priceType": "LIMIT"}, env=MC, parity=["N-common-18"])
node(F, "optionsOrder", "broker_rejects", OO, env=MC, script=[reply("expiry", EXPIRY), reply("optionsorder", BROKER_REJECT)],
     parity=["N-optionsOrder-06"])
node(F, "optionsOrder", "interpolated", {**OO, "underlying": "{{webhook.u}}", "offset": "{{webhook.o}}",
                                         "optionType": "{{webhook.t}}", "quantity": "{{webhook.lots}}"},
     vars_in={"webhook": {"u": "BANKNIFTY", "o": "OTM1", "t": "pe", "lots": "3"}}, env=MC,
     script=[reply("expiry", EXPIRY), reply("optionsorder", OPTIONS_ORDER)], parity=["N-optionsOrder-01"])
node(F, "optionsOrder", "unresolved_underlying", {**OO, "underlying": "{{webhook.u}}"}, env=MC, parity=["N-common-03"])
node(F, "optionsOrder", "editor_defaults", {k: v for k, v in OO.items() if k != "outputVariable"}, env=MC,
     script=[reply("expiry", EXPIRY), reply("optionsorder", OPTIONS_ORDER)], parity=["N-optionsOrder-01"])

# --- optionsMultiOrder ------------------------------------------------------------

MO = {"underlying": "NIFTY", "exchange": "NSE_INDEX", "expiryType": "current_week", "strategy": "straddle",
      "action": "SELL", "quantity": 1, "product": "NRML", "priceType": "MARKET", "outputVariable": "multi"}
MS = [reply("expiry", EXPIRY), reply("optionsmultiorder", MULTI_ORDER)]
P_MO = ["N-optionsMultiOrder-01", "N-optionsMultiOrder-02", "N-optionsMultiOrder-06", "N-optionsMultiOrder-09",
        "N-optionsMultiOrder-10", "N-optionsMultiOrder-11", "CL-18"]
for strat in ("straddle", "strangle", "iron_condor", "bull_call_spread", "bear_put_spread"):
    node(F, "optionsMultiOrder", f"generated_{strat}", {**MO, "strategy": strat}, env=MC, script=MS, parity=P_MO)
node(F, "optionsMultiOrder", "strangle_width", {**MO, "strategy": "strangle", "strangleWidth": "otm3"}, env=MC,
     script=MS, parity=["N-optionsMultiOrder-02", "N-optionsMultiOrder-09"])
node(F, "optionsMultiOrder", "strangle_width_bad", {**MO, "strategy": "strangle", "strangleWidth": "WIDE"}, env=MC,
     parity=["N-optionsMultiOrder-02"])
node(F, "optionsMultiOrder", "strategy_type_alias", {**{k: v for k, v in MO.items() if k != "strategy"},
                                                     "strategyType": "Straddle"}, env=MC, script=MS,
     parity=["N-optionsMultiOrder-02"])
node(F, "optionsMultiOrder", "unknown_strategy", {**MO, "strategy": "butterfly"}, env=MC, script=MS,
     parity=["N-optionsMultiOrder-09", "N-optionsMultiOrder-10"])
node(F, "optionsMultiOrder", "generated_sl_refused", {**MO, "priceType": "SL", "price": 100, "triggerPrice": 99},
     env=MC, parity=["N-optionsMultiOrder-03"])
node(F, "optionsMultiOrder", "generated_limit_without_price", {**MO, "priceType": "LIMIT"}, env=MC,
     parity=["N-optionsMultiOrder-03"])
node(F, "optionsMultiOrder", "generated_limit", {**MO, "priceType": "LIMIT", "price": 120.5}, env=MC, script=MS,
     parity=["N-optionsMultiOrder-09"])
LEGS = [{"strikeMode": "OFFSET", "offset": "ATM", "optionType": "CE", "action": "SELL", "quantity": 1},
        {"strikeMode": "OFFSET", "offset": "OTM2", "optionType": "PE", "action": "BUY", "quantity": 2,
         "priceType": "LIMIT", "price": 45.5}]
CU = {**MO, "strategy": "custom", "legs": LEGS}
node(F, "optionsMultiOrder", "custom_offset_legs", CU, env=MC, script=MS,
     parity=["N-optionsMultiOrder-04", "N-optionsMultiOrder-05", "N-optionsMultiOrder-07", "N-optionsMultiOrder-10"])
node(F, "optionsMultiOrder", "custom_strike_leg",
     {**CU, "legs": [{"strikeMode": "STRIKE", "strike": 24500, "optionType": "CE", "action": "BUY", "quantity": 1},
                     {"strikeMode": "strike", "strike": "24600.5", "optionType": "PE", "action": "SELL", "quantity": 1}]},
     env=MC, script=MS, parity=["N-optionsMultiOrder-05", "N-optionsMultiOrder-07", "N-optionsMultiOrder-10",
                                "N-optionsMultiOrder-12", "C-05"])
node(F, "optionsMultiOrder", "custom_leg_expiry_override",
     {**CU, "legs": [{**LEGS[0], "expiry": "25nov26"}, LEGS[1]]}, env=MC, script=MS,
     parity=["N-optionsMultiOrder-05", "N-optionsMultiOrder-07"])
node(F, "optionsMultiOrder", "custom_leg_expiry_type_override",
     {**CU, "legs": [{**LEGS[0], "expiryType": "next_month"}, {**LEGS[1], "expiryType": "next_month"}]}, env=MC,
     script=MS, parity=["N-optionsMultiOrder-07"], note="Each expiry type is resolved once per node run.")
node(F, "optionsMultiOrder", "custom_leg_expiry_type_unresolvable",
     {**CU, "legs": [{**LEGS[0], "expiryType": "next_week"}]}, env=MC,
     script=[reply("expiry", ok(data=["04-NOV-26"]))], parity=["N-optionsMultiOrder-07"])
node(F, "optionsMultiOrder", "custom_legs_from_variable", {**CU, "legs": "{{webhook.legs}}"},
     vars_in={"webhook": {"legs": LEGS}}, env=MC, script=MS, parity=["N-optionsMultiOrder-04"])
node(F, "optionsMultiOrder", "custom_order_legs_alias", {**{k: v for k, v in CU.items() if k != "legs"}, "orderLegs": LEGS},
     env=MC, script=MS, parity=["N-optionsMultiOrder-04"])
node(F, "optionsMultiOrder", "custom_no_legs", {**{k: v for k, v in CU.items() if k != "legs"}}, env=MC,
     parity=["N-optionsMultiOrder-04", "N-common-13"])
node(F, "optionsMultiOrder", "custom_empty_legs", {**CU, "legs": []}, env=MC, parity=["N-optionsMultiOrder-04"])
node(F, "optionsMultiOrder", "custom_leg_not_object", {**CU, "legs": [LEGS[0], "ATM CE"]}, env=MC,
     parity=["N-optionsMultiOrder-04"])
node(F, "optionsMultiOrder", "imported_without_strategy_is_custom",
     {k: v for k, v in CU.items() if k != "strategy"}, env=MC, script=MS, parity=["N-optionsMultiOrder-02", "V-059"])
for nm, leg in (
    ("leg_bad_offset", {**LEGS[0], "offset": "ATM1"}),
    ("leg_strike_zero", {"strikeMode": "STRIKE", "strike": 0, "optionType": "CE", "action": "BUY", "quantity": 1}),
    ("leg_strike_missing", {"strikeMode": "STRIKE", "optionType": "CE", "action": "BUY", "quantity": 1}),
    ("leg_bad_strike_mode", {**LEGS[0], "strikeMode": "DELTA"}),
    ("leg_bad_expiry", {**LEGS[0], "expiry": "25-11-2026"}),
    ("leg_bad_expiry_type", {**LEGS[0], "expiryType": "weekly"}),
    ("leg_missing_option_type", {k: v for k, v in LEGS[0].items() if k != "optionType"}),
    ("leg_bad_action", {**LEGS[0], "action": "SHORT"}),
    ("leg_quantity_zero", {**LEGS[0], "quantity": 0}),
    ("leg_quantity_missing", {k: v for k, v in LEGS[0].items() if k != "quantity"}),
    ("leg_sl_without_trigger", {**LEGS[0], "priceType": "SL", "price": 100}),
    ("leg_split_size_negative", {**LEGS[0], "splitSize": -5}),
    ("leg_expiry_not_text", {**LEGS[0], "expiry": 251126}),
):
    node(F, "optionsMultiOrder", f"custom_{nm}", {**CU, "legs": [leg]}, env=MC,
         parity=["N-optionsMultiOrder-05", "N-common-15"])
node(F, "optionsMultiOrder", "custom_leg_aliases",
     {**CU, "legs": [{**LEGS[0], "pricetype": "SL", "price": 100, "trigger_price": 99, "splitsize": 25}]},
     env=MC, script=MS, parity=["N-optionsMultiOrder-05"])
node(F, "optionsMultiOrder", "explicit_expiry_date", {**MO, "expiryDate": "25NOV26"}, env=MC,
     script=[reply("optionsmultiorder", MULTI_ORDER)], parity=["N-optionsMultiOrder-02", "N-optionsMultiOrder-06"])
node(F, "optionsMultiOrder", "bad_expiry_date", {**MO, "expiryDate": "Nov 25"}, env=MC, parity=["N-optionsMultiOrder-02"])
node(F, "optionsMultiOrder", "node_expiry_unresolvable", {**MO, "expiryType": "next_week"}, env=MC,
     script=[reply("expiry", ok(data=["04-NOV-26"]))], parity=["N-optionsMultiOrder-06"])
node(F, "optionsMultiOrder", "lot_size_unavailable", {**MO, "underlying": "TATAMOTORS"}, env={"master_contract": NIFTY_CONTRACT},
     parity=["N-optionsMultiOrder-06"])
node(F, "optionsMultiOrder", "mcx_crudeoil_custom", {**CU, "underlying": "CRUDEOIL", "exchange": "MCX"}, env=MC,
     script=[reply("expiry", ok(data=["17-NOV-26", "16-DEC-26"])), reply("optionsmultiorder", MULTI_ORDER)],
     parity=["N-optionsOrder-07", "N-common-20"])
node(F, "optionsMultiOrder", "broker_rejects", {**MO}, env=MC,
     script=[reply("expiry", EXPIRY), reply("optionsmultiorder", BROKER_REJECT)], parity=["N-optionsMultiOrder-11"])
node(F, "optionsMultiOrder", "unresolved_legs", {**CU, "legs": "{{webhook.legs}}"}, env=MC, parity=["N-common-03"])

# --- modifyOrder ------------------------------------------------------------------

MD = {"orderId": "250408000123456", "newPrice": 805.25, "outputVariable": "modified"}
node(F, "modifyOrder", "happy_new_price", MD, script=[reply("orderstatus", ORDER_STATUS), reply("modifyorder", MODIFY)],
     parity=["N-modifyOrder-01", "N-modifyOrder-03", "N-modifyOrder-04", "N-modifyOrder-05", "N-modifyOrder-06", "CL-06"])
node(F, "modifyOrder", "order_id_from_variable", {**MD, "orderId": "{{orderResult.orderid}}"},
     vars_in={"orderResult": {"status": "success", "orderid": "250408000123456"}},
     script=[reply("orderstatus", ORDER_STATUS), reply("modifyorder", MODIFY)], parity=["N-modifyOrder-01"])
node(F, "modifyOrder", "overrides_and_aliases",
     {"orderId": "250408000123456", "symbol": "sbin", "exchange": "NSE", "action": "sell", "product": "CNC",
      "pricetype": "limit", "quantity": "20", "price": "810", "triggerPrice": " "},
     script=[reply("orderstatus", ORDER_STATUS), reply("modifyorder", MODIFY)],
     parity=["N-modifyOrder-01", "N-modifyOrder-04", "N-common-24"])
node(F, "modifyOrder", "missing_order_id", {"newPrice": 805}, parity=["N-modifyOrder-02"])
node(F, "modifyOrder", "status_read_fails", MD, script=[reply("orderstatus", SESSION_EXPIRED)], parity=["N-modifyOrder-03"])
node(F, "modifyOrder", "order_not_found", MD, script=[reply("orderstatus", ok(data={}))], parity=["N-modifyOrder-03"])
node(F, "modifyOrder", "status_missing_identity", MD,
     script=[reply("orderstatus", ok(data={"orderid": "250408000123456", "symbol": "SBIN", "exchange": "NSE",
                                           "quantity": 10, "price": 800.0}))], parity=["N-modifyOrder-04"])
node(F, "modifyOrder", "non_numeric_price", {**MD, "newPrice": "abc"}, script=[reply("orderstatus", ORDER_STATUS)],
     parity=["N-modifyOrder-05"])
node(F, "modifyOrder", "non_numeric_quantity", {**MD, "newQuantity": "1e"}, script=[reply("orderstatus", ORDER_STATUS)],
     parity=["N-modifyOrder-05"])
node(F, "modifyOrder", "quantity_zero", {**MD, "newQuantity": "0"}, script=[reply("orderstatus", ORDER_STATUS)],
     parity=["N-modifyOrder-05"])
node(F, "modifyOrder", "limit_to_zero_reaches_service", {**MD, "newPrice": "0"},
     script=[reply("orderstatus", ORDER_STATUS), reply("modifyorder", MODIFY)], parity=["N-modifyOrder-07"])
node(F, "modifyOrder", "broker_rejects", MD, script=[reply("orderstatus", ORDER_STATUS), reply("modifyorder", BROKER_REJECT)],
     parity=["N-modifyOrder-06"])
node(F, "modifyOrder", "unresolved_order_id", {**MD, "orderId": "{{orderResult.orderid}}"}, parity=["N-common-03"])
node(F, "modifyOrder", "editor_default_only_order_id", {"orderId": "250408000123456"},
     script=[reply("orderstatus", ORDER_STATUS), reply("modifyorder", MODIFY)], parity=["N-modifyOrder-01"])

# --- cancelOrder / cancelAllOrders / closePositions ------------------------------

node(F, "cancelOrder", "happy", {"orderId": "{{orderResult.orderid}}"},
     vars_in={"orderResult": {"orderid": "250408000123456"}}, script=[reply("cancelorder", CANCEL)],
     parity=["N-cancelOrder-01", "N-cancelOrder-02", "CL-07"])
node(F, "cancelOrder", "empty_order_id_reaches_service", {"orderId": ""},
     script=[reply("cancelorder", err("Order ID is missing", 400))], parity=["N-cancelOrder-01"])
node(F, "cancelOrder", "broker_rejects", {"orderId": "250408000123456"},
     script=[reply("cancelorder", err("Order is already complete", 400))], parity=["N-cancelOrder-02"])
node(F, "cancelOrder", "output_variable_not_stored", {"orderId": "250408000123456", "outputVariable": "cancelled"},
     script=[reply("cancelorder", CANCEL)], parity=["N-cancelOrder-02"],
     desktop_expected={"parity": "N-cancelOrder-02", "why": "WEB-DEFECT: cancelOrder never stores its result; the desktop stores it like every other action node.",
                       "fix": ["cancel_store_output"]})
node(F, "cancelOrder", "unresolved_order_id", {"orderId": "{{orderResult.orderid}}"}, parity=["N-common-03"])
node(F, "cancelAllOrders", "happy", {"outputVariable": "cancelAll"}, script=[reply("cancelallorder", CANCEL_ALL)],
     parity=["N-cancelAllOrders-01", "CL-08"])
node(F, "cancelAllOrders", "broker_rejects", {}, script=[reply("cancelallorder", SESSION_EXPIRED)],
     parity=["N-cancelAllOrders-01"])
node(F, "cancelAllOrders", "not_gated_on_unresolved", {"outputVariable": "{{x}}", "symbol": "{{nope}}"},
     script=[reply("cancelallorder", CANCEL_ALL)], parity=["N-cancelAllOrders-01", "N-common-01"])
node(F, "closePositions", "close_all", {"outputVariable": "closed"}, script=[reply("closeposition", CLOSE)],
     parity=["N-closePositions-01", "N-closePositions-02", "CL-09"])
node(F, "closePositions", "close_all_blank_symbol", {"symbol": "  ", "exchange": "NSE"},
     script=[reply("closeposition", CLOSE)], parity=["N-closePositions-02", "N-common-24"])
SCOPED_SQUAREOFF = {"strategy": "Golden Flow", "symbol": "SBIN", "exchange": "NSE", "action": "SELL", "quantity": 0,
                    "position_size": 0, "pricetype": "MARKET", "product": "MIS", "price": 0.0, "trigger_price": 0.0}
node(F, "closePositions", "scoped_symbol", {"symbol": "SBIN", "exchange": "NSE", "outputVariable": "closed"},
     script=[reply("closeposition", ORDER)],
     parity=["N-closePositions-03", "CL-09"],
     desktop_expected={
         "parity": "N-closePositions-03",
         # A note only: the node sends the web's payload and the desktop
         # client routes it (nodes-orders, f6106ffb).
         "why": "WEB-DEFECT CL-09: the web's live close ignores the symbol and squares off every position. The desktop node calls the client's close_position as the web does (same payload); the desktop client sends a call that names a symbol to an internal entry that closes only that position with a smart order to position size 0, under the position's lock, with Semi-Auto refusing it as it refuses closeposition (src-tauri/src/flow/nodes/internal.rs, services/order_service.rs close_one_position; pinned by flow::nodes::internal::tests).",
     })
node(F, "closePositions", "scoped_default_product_nfo", {"symbol": "NIFTY25NOV2624000CE", "exchange": "NFO"},
     script=[reply("closeposition", CLOSE)], parity=["N-closePositions-03", "C-08"])
node(F, "closePositions", "scoped_explicit_product", {"symbol": "SBIN", "exchange": "NSE", "product": "CNC",
                                                      "strategyTag": "swing"},
     script=[reply("closeposition", CLOSE)], parity=["N-closePositions-03"])
node(F, "closePositions", "scoped_no_exchange", {"symbol": "sbin"}, script=[reply("closeposition", CLOSE)],
     parity=["N-closePositions-03"], note="Exchange defaults to NSE; the symbol is not upper-cased here.")
node(F, "closePositions", "broker_rejects", {}, script=[reply("closeposition", err("Close position operation is not allowed in Semi-Auto mode. Please switch to Auto mode to close positions.", 403))],
     parity=["N-closePositions-02", "CL-09"])
node(F, "closePositions", "unresolved_symbol", {"symbol": "{{webhook.symbol}}"}, parity=["N-common-03"])

# --- basketOrder ------------------------------------------------------------------

BO = {"orders": "SBIN,NSE,BUY,10\nINFY,NSE,SELL,5", "basketName": "morning", "priceType": "MARKET",
      "price": 0, "triggerPrice": 0, "outputVariable": "basket"}
node(F, "basketOrder", "csv_rows", BO, script=[reply("basketorder", BASKET)],
     parity=["N-basketOrder-01", "N-basketOrder-03", "N-basketOrder-05", "CL-10"])
node(F, "basketOrder", "default_basket_name", {k: v for k, v in BO.items() if k != "basketName"},
     script=[reply("basketorder", BASKET)], parity=["N-basketOrder-01"])
node(F, "basketOrder", "csv_blank_lines_and_default_products", {**BO, "orders": "\nSBIN,NSE,BUY,10\n\nNIFTY25NOV2624000CE,NFO,SELL,65\n"},
     script=[reply("basketorder", BASKET)], parity=["N-basketOrder-03", "N-basketOrder-04", "C-08"])
node(F, "basketOrder", "csv_lowercase_symbol", {**BO, "orders": "sbin,nse,buy,10"},
     script=[reply("basketorder", BASKET)], parity=["N-basketOrder-04"])
node(F, "basketOrder", "object_rows_from_variable", {**BO, "orders": "{{webhook.orders}}"},
     vars_in={"webhook": {"orders": [
         {"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 10},
         {"symbol": "INFY", "exchange": "NSE", "action": "SELL", "quantity": "5", "product": "CNC",
          "priceType": "LIMIT", "price": 1880.5}]}},
     script=[reply("basketorder", BASKET)], parity=["N-basketOrder-01", "N-basketOrder-03"])
node(F, "basketOrder", "object_row_sl_trigger", {**BO, "orders": [
    {"symbol": "SBIN", "exchange": "NSE", "action": "SELL", "quantity": 10, "pricetype": "SL", "price": 795,
     "triggerprice": 796}]},
     script=[reply("basketorder", BASKET)], parity=["N-basketOrder-06"])
node(F, "basketOrder", "node_level_sl", {**BO, "orders": "SBIN,NSE,SELL,10", "priceType": "SL", "price": 795,
                                         "triggerPrice": 796}, script=[reply("basketorder", BASKET)],
     parity=["N-basketOrder-06"])
node(F, "basketOrder", "row_too_short", {**BO, "orders": "SBIN,NSE,BUY,10\nINFY,NSE,SELL"}, parity=["N-basketOrder-03"])
node(F, "basketOrder", "row_not_object", {**BO, "orders": [{"symbol": "SBIN", "exchange": "NSE", "action": "BUY",
                                                            "quantity": 1}, "INFY,NSE,BUY,1"]},
     parity=["N-basketOrder-03"])
node(F, "basketOrder", "no_rows", {**BO, "orders": "\n  \n"}, parity=["N-basketOrder-03"])
node(F, "basketOrder", "empty_list", {**BO, "orders": []}, parity=["N-basketOrder-03"])
for nm, extra in (
    ("node_product_blank", {"product": " "}),
    ("node_product_invalid", {"product": "BO"}),
    ("node_price_type_invalid", {"priceType": "STOPLOSS"}),
    ("node_price_boolean", {"price": True}),
    ("node_price_blank", {"price": " "}),
    ("node_price_text", {"price": "cheap"}),
    ("node_trigger_not_finite", {"triggerPrice": "nan"}),
    ("node_price_resolves_to_unresolved", {"price": "{{level}}"}),
):
    node(F, "basketOrder", nm, {**BO, **extra}, vars_in={"level": "{{other}}"} if "unresolved" in nm else None,
         parity=["N-basketOrder-02"])
for nm, rows in (
    ("row_symbol_missing", [{"exchange": "NSE", "action": "BUY", "quantity": 1}]),
    ("row_quantity_zero", "SBIN,NSE,BUY,0"),
    ("row_quantity_text", "SBIN,NSE,BUY,ten"),
    ("row_bad_exchange", "SBIN,LSE,BUY,1"),
    ("row_bad_action", "SBIN,NSE,HOLD,1"),
    ("row_bad_product", [{"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 1, "product": "BO"}]),
    ("row_bad_price_type", [{"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 1, "pricetype": "IOC"}]),
    ("row_limit_without_price", [{"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 1, "pricetype": "LIMIT"}]),
    ("row_price_text", [{"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 1, "price": "x"}]),
    ("row_symbol_unresolved", [{"symbol": "{{pick}}", "exchange": "NSE", "action": "BUY", "quantity": 1}]),
):
    node(F, "basketOrder", nm, {**BO, "orders": rows}, parity=["N-basketOrder-04"])
node(F, "basketOrder", "broker_rejects", BO, script=[reply("basketorder", SESSION_EXPIRED)], parity=["N-basketOrder-05"])
node(F, "basketOrder", "unresolved_orders", {**BO, "orders": "{{webhook.orders}}"}, parity=["N-common-03"])

# --- splitOrder -------------------------------------------------------------------

SP = {"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 100, "splitSize": 50, "priceType": "MARKET",
      "price": 0, "triggerPrice": 0, "outputVariable": "split"}
node(F, "splitOrder", "happy", SP, script=[reply("splitorder", SPLIT)],
     parity=["N-splitOrder-01", "N-splitOrder-02", "CL-11"])
node(F, "splitOrder", "default_split_size", {k: v for k, v in SP.items() if k != "splitSize"},
     script=[reply("splitorder", SPLIT)], parity=["N-splitOrder-01"])
node(F, "splitOrder", "split_size_zero", {**SP, "splitSize": 0}, parity=["N-splitOrder-01", "N-common-14"])
node(F, "splitOrder", "quantity_zero", {**SP, "quantity": 0}, parity=["N-splitOrder-01"])
node(F, "splitOrder", "limit_without_price", {**SP, "priceType": "LIMIT"}, parity=["N-splitOrder-02"])
node(F, "splitOrder", "broker_rejects", SP, script=[reply("splitorder", BROKER_REJECT)], parity=["N-splitOrder-02"])
node(F, "splitOrder", "default_product_bfo", {**SP, "exchange": "BFO", "symbol": "SENSEX27NOV2680000CE"},
     script=[reply("splitorder", SPLIT)], parity=["C-08"])
node(F, "splitOrder", "unresolved_split_size", {**SP, "splitSize": "{{n}}"}, parity=["N-common-03"])


# Every basket golden: the desktop sends `trigger_price` (N-basketOrder-06) and
# upper-cases row symbols (N-basketOrder-04); desktop_expected is written only
# where that changes what the web did.
import lib as _lib  # noqa: E402



def apply_auto():
    for _s in _lib.SCENARIOS:
        if _s["type"] == "basketOrder" and not _s["desktop_expected"]:
            _apply_basket(_s)


def _apply_basket(_s):
    if True:
        _s["desktop_expected"] = {
            "parity": "N-basketOrder-06",
            "why": "WEB-DEFECTs N-basketOrder-06 (rows go out as `triggerprice`, which the service never reads, so an SL row's trigger is lost) and N-basketOrder-04 (row symbols are not upper-cased): the desktop sends `trigger_price` and upper-cases the symbol.",
            "fix": ["basket_trigger_price", "basket_upper_symbol"],
        }

# --- {{...}} in the remaining fields ----------------------------------------------

_W = {"webhook": {"symbol": "INFY", "qty": "4", "size": "-2", "split": "2", "px": "1880.5", "tp": "1879",
                  "ex": "NSE", "prod": "CNC", "pt": "SL", "tag": "tv-alert", "id": "250408000123456"}}
node(F, "smartOrder", "interpolated", {"symbol": "{{webhook.symbol}}", "exchange": "{{webhook.ex}}", "action": "SELL",
                                       "quantity": "{{webhook.qty}}", "positionSize": "{{webhook.size}}",
                                       "product": "{{webhook.prod}}", "priceType": "{{webhook.pt}}",
                                       "price": "{{webhook.px}}", "triggerPrice": "{{webhook.tp}}",
                                       "strategyTag": "{{webhook.tag}}"},
     vars_in=_W, script=[reply("placesmartorder", ORDER)], parity=["N-smartOrder-01", "N-common-13"])
node(F, "splitOrder", "interpolated", {"symbol": "{{webhook.symbol}}", "exchange": "NSE", "action": "BUY",
                                       "quantity": "{{webhook.qty}}", "splitSize": "{{webhook.split}}", "priceType": "MARKET"},
     vars_in=_W, script=[reply("splitorder", SPLIT)], parity=["N-splitOrder-01", "N-common-13"])
node(F, "closePositions", "interpolated", {"symbol": "{{webhook.symbol}}", "exchange": "{{webhook.ex}}",
                                           "product": "{{webhook.prod}}"},
     vars_in=_W, script=[reply("closeposition", CLOSE)], parity=["N-closePositions-03", "N-common-24"])
node(F, "cancelOrder", "interpolated_text_around", {"orderId": "id-{{webhook.id}}"}, vars_in=_W,
     script=[reply("cancelorder", CANCEL)], parity=["N-cancelOrder-01"])
node(F, "modifyOrder", "interpolated_overrides", {"orderId": "{{webhook.id}}", "newQuantity": "{{webhook.qty}}",
                                                  "newPrice": "{{webhook.px}}", "newTriggerPrice": "{{webhook.tp}}",
                                                  "priceType": "{{webhook.pt}}"},
     vars_in=_W, script=[reply("orderstatus", ORDER_STATUS), reply("modifyorder", MODIFY)], parity=["N-modifyOrder-01", "N-common-24"])
node(F, "optionsMultiOrder", "interpolated_node_fields", {**MO, "underlying": "{{u}}", "quantity": "{{lots}}",
                                                          "strategy": "{{strat}}", "expiryDate": "{{exp}}"},
     vars_in={"u": "banknifty", "lots": "2", "strat": "STRADDLE", "exp": "25nov26"}, env=MC,
     script=[reply("optionsmultiorder", MULTI_ORDER)], parity=["N-optionsMultiOrder-02"])
