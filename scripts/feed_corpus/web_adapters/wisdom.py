"""Wisdom Capital: the standard XTS template (see xts_family)."""

from web_adapters.xts_family import XtsGlue, sources


class Wisdom(XtsGlue):
    broker = "wisdom"
    prefix = "Wisdom"
    sources = sources("wisdom", "Wisdom")
