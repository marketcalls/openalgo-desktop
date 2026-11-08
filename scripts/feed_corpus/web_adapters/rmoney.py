"""RMoney: its own payload reader, 1501 for LTP and Quote. See xts_family."""

from web_adapters.xts_family import XtsGlue, sources


class Rmoney(XtsGlue):
    broker = "rmoney"
    prefix = "RMoney"
    sources = sources("rmoney", "RMoney")
    numeric_instrument_id = True
