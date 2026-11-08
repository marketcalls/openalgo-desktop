"""One module per broker: which web methods turn its frames into what the
web publishes, and the state they read (`webkit.Glue`). A broker is added
here by its module and a line in GLUES."""

from web_adapters.angel import Angel
from web_adapters.dhan import Dhan
from web_adapters.fyers import Fyers
from web_adapters.zerodha import Zerodha

GLUES = {g.broker: g for g in (Angel, Dhan, Fyers, Zerodha)}
