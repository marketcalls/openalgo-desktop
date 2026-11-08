"""Jainam XTS: the event name supplies a missing MessageCode, 1105 text is
1512, and each message goes to every mode the symbol's subscriptions
hold. See xts_family."""

from web_adapters.xts_family import XtsGlue, sources


class Jainamxts(XtsGlue):
    broker = "jainamxts"
    prefix = "JainamXTS"
    sources = sources("jainamxts", "JainamXTS")
