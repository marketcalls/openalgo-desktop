"""IIFL XTS (not IIFL Capital): the standard XTS template (see xts_family)."""

from web_adapters.xts_family import XtsGlue, sources


class Iifl(XtsGlue):
    broker = "iifl"
    prefix = "Iifl"
    sources = sources("iifl", "Iifl")
