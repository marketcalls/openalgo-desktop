"""HDFC Securities InvestRight (`broker/hdfcsecurities/streaming/`): see
`hdfc_common`. Its adapter keeps the merge baselines across the client's
own reconnect (`_on_ws_connect` and `_on_ws_disconnect` change only
`connected`)."""

from web_adapters.hdfc_common import HdfcGlue, sources


class HdfcSecurities(HdfcGlue):
    broker = "hdfcsecurities"
    pb2 = "broker/hdfcsecurities/streaming/hdfcsecurities_market_pb2.py"
    client = "HDFCSecuritiesWebSocket"
    adapter = "HDFCSecuritiesWebSocketAdapter"
    sources = sources("hdfcsecurities", "HDFCSecuritiesCapabilityRegistry", client, adapter, [])
