"""One module per broker: which web methods turn its frames into what the
web publishes, and the state they read (`webkit.Glue`). A broker is added
here by its module and a line in GLUES."""

from web_adapters.aliceblue import AliceBlue
from web_adapters.angel import Angel
from web_adapters.compositedge import Compositedge
from web_adapters.dhan import Dhan
from web_adapters.firstock import Firstock
from web_adapters.fivepaisaxts import Fivepaisaxts
from web_adapters.flattrade import Flattrade
from web_adapters.ibulls import Ibulls
from web_adapters.iifl import Iifl
from web_adapters.shoonya import Shoonya
from web_adapters.tradesmart import TradeSmart
from web_adapters.wisdom import Wisdom
from web_adapters.zebu import Zebu
from web_adapters.zerodha import Zerodha

GLUES = {
    g.broker: g
    for g in (
        AliceBlue,
        Angel,
        Compositedge,
        Dhan,
        Firstock,
        Fivepaisaxts,
        Flattrade,
        Ibulls,
        Iifl,
        Shoonya,
        TradeSmart,
        Wisdom,
        Zebu,
        Zerodha,
    )
}
