"""IBulls: the standard XTS template (see xts_family)."""

from web_adapters.xts_family import XtsGlue, sources


class Ibulls(XtsGlue):
    broker = "ibulls"
    prefix = "Ibulls"
    sources = sources("ibulls", "Ibulls")
