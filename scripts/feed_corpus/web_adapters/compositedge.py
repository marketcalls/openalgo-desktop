"""CompositEdge: the standard XTS template (see xts_family)."""

from web_adapters.xts_family import XtsGlue, sources


class Compositedge(XtsGlue):
    broker = "compositedge"
    prefix = "Compositedge"
    sources = sources("compositedge", "Compositedge")
