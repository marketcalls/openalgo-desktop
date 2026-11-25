"""5paisa XTS: the template with its fixes; an unsubscribe keeps the
connection (fivepaisaxts_adapter.py `unsubscribe`). See xts_family.

Since web #2504 (5749d4126) the client keeps what the server confirmed,
one entry per `(xts code, segment, instrument id)` (`_remember`), so its
book no longer has the adapter's correlation ids as keys, and the adapter's
`unsubscribe` calls the client's whenever it is connected: the replay does
the same, so the client's 1105 filter forgets the instrument as the web's
does. The adapter now reads `Touchline` whatever the message code.
"""

from web_adapters.xts_family import XtsGlue, sources


class Fivepaisaxts(XtsGlue):
    broker = "fivepaisaxts"
    prefix = "FivepaisaXTS"
    sources = sources("fivepaisaxts", "FivepaisaXTS")
    disconnects_on_unsubscribe = False

    def send_unsubscribe(self, key, mode, instruments):
        self.ws.unsubscribe(key, mode, instruments)
