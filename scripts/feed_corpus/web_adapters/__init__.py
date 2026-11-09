"""One module per broker: which web methods turn its frames into what the
web publishes, and the state they read (`webkit.Glue`). A broker is added
here by its module and a line in GLUES."""

from web_adapters.aliceblue import AliceBlue
from web_adapters.angel import Angel
from web_adapters.arrow import Arrow
from web_adapters.compositedge import Compositedge
from web_adapters.definedge import Definedge
from web_adapters.deltaexchange import DeltaExchange
from web_adapters.dhan import Dhan
from web_adapters.dhan_sandbox import DhanSandbox
from web_adapters.firstock import Firstock
from web_adapters.fivepaisa import Fivepaisa
from web_adapters.fivepaisaxts import Fivepaisaxts
from web_adapters.flattrade import Flattrade
from web_adapters.fyers import Fyers
from web_adapters.groww import Groww
from web_adapters.hdfcsecurities import HdfcSecurities
from web_adapters.hdfcsky import HdfcSky
from web_adapters.ibulls import Ibulls
from web_adapters.iifl import Iifl
from web_adapters.iiflcapital import IiflCapital
from web_adapters.indmoney import Indmoney
from web_adapters.jainamxts import Jainamxts
from web_adapters.kotak import Kotak
from web_adapters.motilal import Motilal
from web_adapters.mstock import Mstock
from web_adapters.nubra import Nubra
from web_adapters.paytm import Paytm
from web_adapters.pocketful import Pocketful
from web_adapters.rmoney import Rmoney
from web_adapters.rupeezy import Rupeezy
from web_adapters.sahi import Sahi
from web_adapters.samco import Samco
from web_adapters.shoonya import Shoonya
from web_adapters.tradejini import Tradejini
from web_adapters.tradesmart import TradeSmart
from web_adapters.upstox import Upstox
from web_adapters.wisdom import Wisdom
from web_adapters.zebu import Zebu
from web_adapters.zerodha import Zerodha

GLUES = {g.broker: g for g in (AliceBlue, Angel, Arrow, Compositedge, Definedge, DeltaExchange, Dhan, DhanSandbox, Firstock, Fivepaisa, Fivepaisaxts, Flattrade, Fyers, Groww, HdfcSecurities, HdfcSky, Ibulls, Iifl, IiflCapital, Indmoney, Jainamxts, Kotak, Motilal, Mstock, Nubra, Paytm, Pocketful, Rmoney, Rupeezy, Sahi, Samco, Shoonya, TradeSmart, Tradejini, Upstox, Wisdom, Zebu, Zerodha)}
