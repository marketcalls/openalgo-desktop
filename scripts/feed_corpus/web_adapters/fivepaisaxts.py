"""5paisa XTS: the template with its fixes; an unsubscribe keeps the
connection (fivepaisaxts_adapter.py `unsubscribe`). See xts_family."""

from web_adapters.xts_family import XtsGlue, sources


class Fivepaisaxts(XtsGlue):
    broker = "fivepaisaxts"
    prefix = "FivepaisaXTS"
    sources = sources("fivepaisaxts", "FivepaisaXTS")
    disconnects_on_unsubscribe = False
