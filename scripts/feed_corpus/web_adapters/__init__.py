"""One module per broker: which web methods turn its frames into what the
web publishes, and the state they read (`webkit.Glue`). A broker is added
here by its module and a line in GLUES."""

from web_adapters.angel import Angel
from web_adapters.arrow import Arrow
from web_adapters.definedge import Definedge
from web_adapters.deltaexchange import DeltaExchange
from web_adapters.dhan import Dhan
from web_adapters.dhan_sandbox import DhanSandbox
from web_adapters.fivepaisa import Fivepaisa
from web_adapters.fyers import Fyers
from web_adapters.groww import Groww
from web_adapters.hdfcsecurities import HdfcSecurities
from web_adapters.hdfcsky import HdfcSky
from web_adapters.iiflcapital import IiflCapital
from web_adapters.indmoney import Indmoney
from web_adapters.kotak import Kotak
from web_adapters.motilal import Motilal
from web_adapters.mstock import Mstock
from web_adapters.nubra import Nubra
from web_adapters.paytm import Paytm
from web_adapters.pocketful import Pocketful
from web_adapters.rupeezy import Rupeezy
from web_adapters.sahi import Sahi
from web_adapters.samco import Samco
from web_adapters.tradejini import Tradejini
from web_adapters.upstox import Upstox
from web_adapters.zerodha import Zerodha

GLUES = {g.broker: g for g in (Angel, Arrow, Definedge, DeltaExchange, Dhan, DhanSandbox, Fivepaisa, Fyers, Groww, HdfcSecurities, HdfcSky, IiflCapital, Indmoney, Kotak, Motilal, Mstock, Nubra, Paytm, Pocketful, Rupeezy, Sahi, Samco, Tradejini, Upstox, Zerodha)}
