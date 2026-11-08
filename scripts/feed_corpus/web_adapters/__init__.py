"""One module per broker: which web methods turn its frames into what the
web publishes, and the state they read (`webkit.Glue`). A broker is added
here by its module and a line in GLUES."""

from web_adapters.angel import Angel
from web_adapters.definedge import Definedge
from web_adapters.dhan import Dhan
from web_adapters.fivepaisa import Fivepaisa
from web_adapters.motilal import Motilal
from web_adapters.mstock import Mstock
from web_adapters.samco import Samco
from web_adapters.tradejini import Tradejini
from web_adapters.zerodha import Zerodha

GLUES = {g.broker: g for g in (Angel, Definedge, Dhan, Fivepaisa, Motilal, Mstock, Samco, Tradejini, Zerodha)}
