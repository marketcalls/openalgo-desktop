"""One module per broker: which web methods turn its frames into what the
web publishes, and the state they read (`webkit.Glue`). A broker is added
here by its module and a line in GLUES."""

from web_adapters.angel import Angel
from web_adapters.arrow import Arrow
from web_adapters.dhan import Dhan
from web_adapters.rupeezy import Rupeezy
from web_adapters.zerodha import Zerodha

GLUES = {g.broker: g for g in (Angel, Arrow, Dhan, Rupeezy, Zerodha)}
