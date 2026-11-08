"""HDFC Sky (`broker/hdfcsky/streaming/`): see `hdfc_common`. The adapter
keys each subscription by a generation and the connection by an epoch, and
drops its merge baselines on every reconnect (web openalgo#2397)."""

from web_adapters.hdfc_common import HdfcGlue, sources


class HdfcSky(HdfcGlue):
    broker = "hdfcsky"
    pb2 = "broker/hdfcsky/streaming/hdfcsky_market_pb2.py"
    client = "HDFCSkyWebSocket"
    adapter = "HDFCSkyWebSocketAdapter"
    adapter_state = {"_subscription_seq": 0, "_feed_epoch": 0}
    sources = sources("hdfcsky", "HDFCSkyCapabilityRegistry", client, adapter, ["_drop_snapshots"])
