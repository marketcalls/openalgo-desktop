"""Wisdom Capital: the standard XTS template (see xts_family), on the client
web #2469 (11b53225e) rebuilt.

The adapter decodes and normalises as the template does; what changed is
the client around it. Each connection builds its own `socketio.Client` in
`_new_socketio(ended)` (handlers registered there), subscribes go through
`subscribe_batch(mode, instruments)` (at most 50 instruments of one mode per
request, a refused request of several sent again one at a time) and are
remembered per `(xts code, segment, instrument id)`, `unsubscribe(mode,
instruments)` takes the mode, and the adapter's `unsubscribe` no longer
disconnects the feed (wisdom_adapter.py `unsubscribe`). The batch sender
thread (`_send_pending_subscriptions`) only queues and times the calls; the
replay calls `subscribe_batch` at once, which is what it would send.
"""

import threading

from web_adapters.xts_family import XtsGlue, sources


class Wisdom(XtsGlue):
    broker = "wisdom"
    prefix = "Wisdom"
    sources = sources("wisdom", "Wisdom")
    disconnects_on_unsubscribe = False

    def setup_client(self):
        self.sio = self.ws._new_socketio(threading.Event())

    def socket_handlers(self):
        return self.sio.handlers

    def send_subscribe(self, cid, mode, instruments):
        self.ws.subscribe_batch(mode, instruments)

    def send_unsubscribe(self, key, mode, instruments):
        self.ws.unsubscribe(mode, instruments)
