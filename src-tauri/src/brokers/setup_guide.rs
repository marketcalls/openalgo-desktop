//! What the Broker Configuration page asks for, per broker: the keys each
//! broker needs, labelled the way that broker names them, a hint for each,
//! and a sentence on how its sign-in works.
//!
//! Desktop-only (an approved deviation from the web, which reads one
//! broker's keys from `.env`). The pages keep no broker list of their own:
//! `GET /api/broker/list` serves this table with the catalogue's sign-in
//! kind, so a new broker needs an entry here and nothing in TypeScript.
//!
//! Sources, checked broker by broker: how each adapter reads
//! `BrokerCredentials` (`api_key`, `api_secret`, the XTS market-data pair,
//! the client id), `catalog::login_fields` and `CLIENT_ID_BROKERS`, the
//! web's `blueprints/brlogin.py`, `broker/<id>/api/auth_api.py` and
//! `utils/env_check.py`, and the web's broker page names. Where a broker's
//! own name for a key could not be confirmed, the generic one is used.

use crate::brokers::catalog::{self, SignIn};

/// A stored key, by its `BrokerCredentials` slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyName {
    /// The web's BROKER_API_KEY.
    ApiKey,
    /// The web's BROKER_API_SECRET.
    ApiSecret,
    /// The account a sign-in must come back for (`CLIENT_ID_BROKERS`).
    ClientId,
    /// The XTS market-data key (BROKER_API_KEY_MARKET).
    ApiKeyMarket,
    /// The XTS market-data secret (BROKER_API_SECRET_MARKET).
    ApiSecretMarket,
}

impl KeyName {
    /// The field name the save endpoint takes.
    pub fn as_str(self) -> &'static str {
        match self {
            KeyName::ApiKey => "api_key",
            KeyName::ApiSecret => "api_secret",
            KeyName::ClientId => "client_id",
            KeyName::ApiKeyMarket => "api_key_market",
            KeyName::ApiSecretMarket => "api_secret_market",
        }
    }
}

/// One key a broker needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyField {
    pub key: KeyName,
    pub label: &'static str,
    pub hint: &'static str,
    /// The replacement input hides what is typed.
    pub secret: bool,
    /// Needed before the broker's first sign-in.
    pub required: bool,
}

/// The Broker Configuration entry for one broker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupGuide {
    /// The name the broker pages show (the web's broker page names).
    pub name: &'static str,
    pub fields: &'static [KeyField],
    /// How this broker's sign-in works, for a trader.
    pub banner: &'static str,
    /// What to do with the redirect URL (redirect brokers only).
    pub redirect_hint: &'static str,
}

const fn key(label: &'static str, hint: &'static str) -> KeyField {
    KeyField {
        key: KeyName::ApiKey,
        label,
        hint,
        secret: false,
        required: true,
    }
}

const fn secret(label: &'static str, hint: &'static str) -> KeyField {
    KeyField {
        key: KeyName::ApiSecret,
        label,
        hint,
        secret: true,
        required: true,
    }
}

const fn optional_secret(label: &'static str, hint: &'static str) -> KeyField {
    KeyField {
        key: KeyName::ApiSecret,
        label,
        hint,
        secret: true,
        required: false,
    }
}

const fn client_id(label: &'static str, hint: &'static str) -> KeyField {
    KeyField {
        key: KeyName::ClientId,
        label,
        hint,
        secret: false,
        required: true,
    }
}

const MARKET_KEY: KeyField = KeyField {
    key: KeyName::ApiKeyMarket,
    label: "Market Data App Key",
    hint:
        "The Market Data API app key from the same XTS account. Quotes and the live feed need it.",
    secret: false,
    required: false,
};

const MARKET_SECRET: KeyField = KeyField {
    key: KeyName::ApiSecretMarket,
    label: "Market Data Secret Key",
    hint: "The Market Data API secret key from the same XTS account.",
    secret: true,
    required: false,
};

const REDIRECT_REGISTER: &str =
    "Register this exact address as the redirect URL in your broker app.";
const REDIRECT_SENT: &str = "OpenAlgo sends this address with each login. If your broker app asks for a redirect URL, register this exact address.";

/// XTS brokers whose saved keys are all a sign-in needs.
const fn xts_saved_keys(name: &'static str, banner: &'static str) -> SetupGuide {
    SetupGuide {
        name,
        fields: const {
            &[
                key(
                    "Interactive App Key",
                    "The Interactive API app key from your XTS account.",
                ),
                secret(
                    "Interactive Secret Key",
                    "The Interactive API secret key from your XTS account.",
                ),
                MARKET_KEY,
                MARKET_SECRET,
            ]
        },
        banner,
        redirect_hint: "",
    }
}

/// The generic entry, for a broker this table does not describe.
const GENERIC_FIELDS: &[KeyField] = &[
    key("API Key", "From your broker's developer portal."),
    secret("API Secret", "From your broker's developer portal."),
];

/// The Broker Configuration entry for `broker`.
pub fn guide(broker: &str) -> SetupGuide {
    guide_a_to_d(broker)
        .or_else(|| guide_d_to_g(broker))
        .or_else(|| guide_h_to_m(broker))
        .or_else(|| guide_m_to_p(broker))
        .or_else(|| guide_r_to_t(broker))
        .or_else(|| guide_t_to_z(broker))
        .unwrap_or_else(|| generic_guide(broker))
}

fn guide_a_to_d(broker: &str) -> Option<SetupGuide> {
    Some(match broker {
        "aliceblue" => SetupGuide {
            name: "Alice Blue",
            fields: const { &[
                key("App Code", "The App Code of your Alice Blue API app."),
                secret("API Secret", "The API secret of your Alice Blue API app."),
                client_id(
                    "User ID",
                    "Your Alice Blue User ID. Each login is checked against it.",
                ),
            ] },
            banner: "Login with Alice Blue takes you to Alice Blue's login page. Log in there and Alice Blue sends you back to OpenAlgo, which checks it is the User ID you saved.",
            redirect_hint: REDIRECT_REGISTER,
        },
        "angel" => SetupGuide {
            name: "Angel One",
            fields: const { &[key(
                "SmartAPI Key",
                "The API key of your SmartAPI app at smartapi.angelbroking.com. Angel One needs no secret.",
            )] },
            banner: "Login with Angel One opens a short form in OpenAlgo. Each time, enter your Angel One Client ID, PIN and the 6-digit TOTP from your authenticator app.",
            redirect_hint: "",
        },
        "arrow" => SetupGuide {
            name: "Arrow",
            fields: const { &[
                key("App ID", "The App ID of your Arrow app (app.arrow.trade)."),
                secret("App Secret", "The App Secret of your Arrow app."),
                client_id(
                    "Client ID",
                    "Your Arrow trading account ID. Each login is checked against it.",
                ),
            ] },
            banner: "Login with Arrow takes you to Arrow's login page. Log in there and Arrow sends you back to OpenAlgo, which checks it is the client ID you saved.",
            redirect_hint: REDIRECT_REGISTER,
        },
        "compositedge" => SetupGuide {
            name: "CompositEdge",
            fields: const { &[
                key(
                    "Interactive App Key",
                    "The Interactive API app key from your CompositEdge XTS account.",
                ),
                secret(
                    "Interactive Secret Key",
                    "The Interactive API secret key from your CompositEdge XTS account.",
                ),
                MARKET_KEY,
                MARKET_SECRET,
            ] },
            banner: "Login with CompositEdge takes you to CompositEdge's login page. Log in there and CompositEdge sends you back to OpenAlgo; your Market Data keys are then used for quotes and the live feed.",
            redirect_hint: REDIRECT_SENT,
        },
        "definedge" => SetupGuide {
            name: "Definedge",
            fields: const { &[
                key("API Token", "The API token from your Definedge Integrate account."),
                secret("API Secret", "The API secret from your Definedge Integrate account."),
            ] },
            banner: "Login with Definedge opens a short form in OpenAlgo and sends a one-time code to your registered mobile or email. Enter that OTP to sign in.",
            redirect_hint: "",
        },
        "deltaexchange" => SetupGuide {
            name: "Delta Exchange",
            fields: const { &[
                key("API Key", "An API key from your Delta Exchange India account."),
                secret("API Secret", "The secret of that API key."),
            ] },
            banner: "Login with Delta Exchange signs you in with one click using the API key and secret you saved; there is nothing to type.",
            redirect_hint: "",
        },
        _ => return None,
    })
}

fn guide_d_to_g(broker: &str) -> Option<SetupGuide> {
    Some(match broker {
        "dhan" => SetupGuide {
            name: "Dhan",
            fields: const { &[
                key(
                    "Client ID and API Key",
                    "Your Dhan Client ID and your app's API key joined by ::: (client_id:::api_key).",
                ),
                secret("API Secret", "The API secret of your Dhan app."),
            ] },
            banner: "Login with Dhan takes you to Dhan's login page. Log in and approve, and Dhan sends you back to OpenAlgo. Market data needs an active Dhan Data API subscription.",
            redirect_hint: REDIRECT_REGISTER,
        },
        "dhan_sandbox" => SetupGuide {
            name: "Dhan (Sandbox)",
            fields: const { &[
                key("Client ID", "Your Dhan Client ID."),
                secret(
                    "Sandbox Access Token",
                    "The access token of your Dhan sandbox account.",
                ),
            ] },
            banner: "Login with Dhan (Sandbox) signs you in with one click using the client ID and sandbox access token you saved. Its live prices are simulated prices, not market prices: Dhan's sandbox streams none, so OpenAlgo makes them up as OpenAlgo web does.",
            redirect_hint: "",
        },
        "firstock" => SetupGuide {
            name: "Firstock",
            fields: const { &[
                key("Vendor Code", "Your Firstock vendor code."),
                secret("API Key", "Your Firstock API key."),
            ] },
            banner: "Login with Firstock opens a short form in OpenAlgo. Each time, enter your Firstock user ID, password and TOTP.",
            redirect_hint: "",
        },
        "fivepaisa" => SetupGuide {
            name: "5 Paisa",
            fields: const { &[
                key(
                    "User Key, User ID and Client Code",
                    "The User Key and User ID of your 5paisa app and your client code, joined by ::: (User_Key:::User_ID:::client_id).",
                ),
                secret("Encryption Key", "The encryption key of your 5paisa app."),
            ] },
            banner: "Login with 5 Paisa opens a short form in OpenAlgo. Each time, enter your 5paisa login, PIN and the TOTP from your authenticator app.",
            redirect_hint: "",
        },
        "fivepaisaxts" => xts_saved_keys(
            "5 Paisa (XTS)",
            "Login with 5 Paisa (XTS) signs you in with one click using your saved Interactive and Market Data keys.",
        ),
        "flattrade" => SetupGuide {
            name: "Flattrade",
            fields: const { &[
                key(
                    "Client ID and API Key",
                    "Your Flattrade client ID and your app's API key joined by ::: (client_id:::api_key).",
                ),
                secret("API Secret", "The API secret of your Flattrade app."),
            ] },
            banner: "Login with Flattrade takes you to Flattrade's login page. Log in there and Flattrade sends you back to OpenAlgo.",
            redirect_hint: REDIRECT_REGISTER,
        },
        "fyers" => SetupGuide {
            name: "Fyers",
            fields: const { &[
                key("App ID", "The App ID of your app at myapi.fyers.in."),
                secret("App Secret", "The secret of that app."),
            ] },
            banner: "Login with Fyers takes you to Fyers' login page. Log in there and Fyers sends you back to OpenAlgo.",
            redirect_hint: REDIRECT_REGISTER,
        },
        "groww" => SetupGuide {
            name: "Groww",
            fields: const { &[
                key("API Key", "Your Groww Trade API key."),
                optional_secret(
                    "API Secret",
                    "Needed when your key uses Groww's approval flow. Leave it empty for a TOTP key.",
                ),
            ] },
            banner: "Login with Groww opens a short form in OpenAlgo. For a TOTP key, enter the current code from your authenticator app; otherwise OpenAlgo signs in with the API key and secret you saved.",
            redirect_hint: "",
        },
        _ => return None,
    })
}

fn guide_h_to_m(broker: &str) -> Option<SetupGuide> {
    Some(match broker {
        "hdfcsecurities" => SetupGuide {
            name: "HDFC Securities",
            fields: const { &[
                key("API Key", "The API key of your HDFC Securities InvestRight app."),
                secret("API Secret", "The API secret of that app."),
                client_id(
                    "Client ID",
                    "Your HDFC Securities client ID. Each login is checked against it.",
                ),
            ] },
            banner: "Login with HDFC Securities takes you to HDFC Securities' login page. Log in there and you are sent back to OpenAlgo, which checks it is the client ID you saved.",
            redirect_hint: REDIRECT_REGISTER,
        },
        "hdfcsky" => SetupGuide {
            name: "HDFC Sky",
            fields: const { &[
                key("API Key", "The API key of your HDFC Sky app."),
                secret("API Secret", "The API secret of that app."),
                client_id(
                    "Client ID",
                    "Your HDFC Sky client ID. Each login is checked against it.",
                ),
            ] },
            banner: "Login with HDFC Sky takes you to HDFC Sky's login page. Log in there and you are sent back to OpenAlgo, which checks it is the client ID you saved.",
            redirect_hint: REDIRECT_REGISTER,
        },
        "ibulls" => xts_saved_keys(
            "Ibulls",
            "Login with Ibulls signs you in with one click using your saved Interactive and Market Data keys.",
        ),
        "iifl" => xts_saved_keys(
            "IIFL",
            "Login with IIFL signs you in with one click using your saved Interactive and Market Data keys.",
        ),
        "iiflcapital" => SetupGuide {
            name: "IIFL Capital",
            fields: const { &[
                key("App Key", "The app key of your IIFL Capital app."),
                secret("App Secret", "The app secret of that app."),
            ] },
            banner: "Login with IIFL Capital takes you to IIFL Capital's login page. Log in there and IIFL sends you back to OpenAlgo.",
            redirect_hint: "Register this exact address as the redirect URL with IIFL Capital; it must be whitelisted on your app.",
        },
        "indmoney" => SetupGuide {
            name: "IndMoney",
            fields: const { &[
                key(
                    "Client ID",
                    "The Client ID shown at indstocks.com, API Trading, Access Tokens, after you set up TOTP there.",
                ),
                optional_secret(
                    "Access Token",
                    "Leave empty to log in with your MPIN and TOTP (recommended). A pasted access token is used as is and lasts 24 hours.",
                ),
            ] },
            banner: "Login with IndMoney opens a short form in OpenAlgo. Each time, enter your INDstocks MPIN and a fresh TOTP; a new code can be requested once a minute.",
            redirect_hint: "",
        },
        "jainamxts" => xts_saved_keys(
            "JainamXts",
            "Login with JainamXts signs you in with one click using your saved Interactive and Market Data keys.",
        ),
        "kotak" => SetupGuide {
            name: "Kotak Securities",
            fields: const { &[
                key("UCC", "Your Kotak Neo Unique Client Code."),
                secret(
                    "Access Token",
                    "The Neo API access token from Kotak's developer portal.",
                ),
            ] },
            banner: "Login with Kotak Securities opens a short form in OpenAlgo. Each time, enter your registered mobile number, the TOTP from the Kotak Neo app and your MPIN.",
            redirect_hint: "",
        },
        "motilal" => SetupGuide {
            name: "Motilal Oswal",
            fields: const { &[
                key("API Key", "The API key of your Motilal Oswal app."),
                optional_secret(
                    "API Secret",
                    "The API secret of that app, if it has one.",
                ),
            ] },
            banner: "Login with Motilal Oswal opens a short form in OpenAlgo. Each time, enter your user ID, password, date of birth and, if you use one, your TOTP.",
            redirect_hint: "",
        },
        _ => return None,
    })
}

fn guide_m_to_p(broker: &str) -> Option<SetupGuide> {
    Some(match broker {
        "mstock" => SetupGuide {
            name: "mStock by Mirae Asset",
            fields: const { &[
                key("Client Code", "Your mStock client code (your login ID)."),
                secret("API Key", "The API key of your mStock app."),
            ] },
            banner: "Login with mStock by Mirae Asset opens a short form in OpenAlgo. Each time, enter your mStock password and TOTP.",
            redirect_hint: "",
        },
        "nubra" => SetupGuide {
            name: "Nubra",
            fields: const { &[
                key(
                    "Registered Mobile Number",
                    "The mobile number registered with your Nubra account.",
                ),
                secret("MPIN", "Your Nubra MPIN."),
            ] },
            banner: "Login with Nubra opens a short form in OpenAlgo and texts a one-time code to your registered mobile. Enter it, and OpenAlgo uses your saved MPIN to finish.",
            redirect_hint: "",
        },
        "paytm" => SetupGuide {
            name: "Paytm Money",
            fields: const { &[
                key("API Key", "The API key of your Paytm Money app."),
                secret("API Secret", "The API secret of that app."),
            ] },
            banner: "Login with Paytm Money takes you to Paytm Money's login page. Log in there and you are sent back to OpenAlgo.",
            redirect_hint: REDIRECT_REGISTER,
        },
        "pocketful" => SetupGuide {
            name: "Pocketful",
            fields: const { &[
                key(
                    "App Client ID",
                    "The client ID of your Pocketful app, not your trading account ID.",
                ),
                secret("Client Secret", "The client secret of that app."),
            ] },
            banner: "Login with Pocketful takes you to Pocketful's login page. Log in and approve, and Pocketful sends you back to OpenAlgo.",
            redirect_hint: REDIRECT_SENT,
        },
        _ => return None,
    })
}

fn guide_r_to_t(broker: &str) -> Option<SetupGuide> {
    Some(match broker {
        "rmoney" => SetupGuide {
            name: "RMoney",
            fields: const { &[
                key(
                    "Interactive App Key",
                    "The Interactive API app key from your RMoney XTS account. RMoney needs no Interactive secret.",
                ),
                MARKET_KEY,
                MARKET_SECRET,
            ] },
            banner: "Login with RMoney takes you to RMoney's login page. Log in there and RMoney sends you back to OpenAlgo; your Market Data keys are then used for quotes and the live feed.",
            redirect_hint: REDIRECT_SENT,
        },
        "rupeezy" => SetupGuide {
            name: "Rupeezy",
            fields: const { &[
                key(
                    "Application ID",
                    "The application ID of your Vortex app on the Rupeezy developer portal (vortex.rupeezy.in).",
                ),
                secret(
                    "API Key",
                    "The API key of that Vortex app. OpenAlgo sends it with every request and uses it to finish each login.",
                ),
                client_id(
                    "Client ID",
                    "Your Rupeezy client code. Each login is checked against it.",
                ),
            ] },
            banner: "Login with Rupeezy takes you to Rupeezy's login page. Log in there and Rupeezy sends you back to OpenAlgo, which checks it is the client ID you saved.",
            redirect_hint: REDIRECT_REGISTER,
        },
        "sahi" => SetupGuide {
            name: "Sahi",
            fields: const { &[
                key(
                    "API Key",
                    "The key id of an API key from Sahi web, Profile, API Dashboard (it starts with pk_). Create it with Full access to place orders.",
                ),
                secret(
                    "API Secret",
                    "The secret shown once when that key was created (it starts with sahi_sk_).",
                ),
            ] },
            banner: "Login with Sahi signs you in with one click using the API key and secret you saved. Approve the key in Sahi's API Dashboard once each trading day first, and register the internet address this computer's traffic leaves from as a static IP on your Sahi account: Sahi refuses orders from any other address.",
            redirect_hint: "",
        },
        "samco" => SetupGuide {
            name: "Samco",
            fields: const { &[
                key(
                    "API Key",
                    "The API key of your Samco app (tradeapi.samco.in), emailed when the app is created.",
                ),
                secret(
                    "API Secret",
                    "The API secret of that app, shown once when the app is created.",
                ),
            ] },
            banner: "Login with Samco opens Samco's connect page. One click signs you in with your saved keys and checks that this computer's internet address is the static IP registered with Samco.",
            redirect_hint: "",
        },
        "shoonya" => SetupGuide {
            name: "Shoonya",
            fields: const { &[
                key(
                    "User ID and Client ID",
                    "Your Shoonya user ID and your app's client ID joined by ::: (userid:::client_id).",
                ),
                secret("Secret Key", "The secret key of your Shoonya app."),
            ] },
            banner: "Login with Shoonya takes you to Shoonya's login page. Log in there and Shoonya sends you back to OpenAlgo.",
            redirect_hint: REDIRECT_REGISTER,
        },
        "tradejini" => SetupGuide {
            name: "Tradejini",
            fields: const { &[key(
                "API Key",
                "The API key of your individual app in the Tradejini developer portal. Tradejini needs no secret; calls must come from the static IP whitelisted on that app.",
            )] },
            banner: "Login with Tradejini opens a short form in OpenAlgo. Each time, enter your CubePlus login PIN and TOTP.",
            redirect_hint: "",
        },
        _ => return None,
    })
}

fn guide_t_to_z(broker: &str) -> Option<SetupGuide> {
    Some(match broker {
        "tradesmart" => SetupGuide {
            name: "TradeSmart",
            fields: const { &[
                key(
                    "User ID and App Key",
                    "Your TradeSmart user ID and your app key joined by ::: (userid:::app_key).",
                ),
                secret("Secret Key", "The secret key of your TradeSmart app."),
            ] },
            banner: "Login with TradeSmart takes you to TradeSmart's login page. Log in there and TradeSmart sends you back to OpenAlgo.",
            redirect_hint: REDIRECT_REGISTER,
        },
        "upstox" => SetupGuide {
            name: "Upstox",
            fields: const { &[
                key("API Key", "The API key of your Upstox app."),
                secret("API Secret", "The API secret of that app."),
            ] },
            banner: "Login with Upstox takes you to Upstox's login page. Log in there and Upstox sends you back to OpenAlgo.",
            redirect_hint: "Register this exact address as the redirect URL in your Upstox app; it must match character for character (localhost is not 127.0.0.1).",
        },
        "wisdom" => xts_saved_keys(
            "Wisdom Capital",
            "Login with Wisdom Capital signs you in with one click using your saved Interactive and Market Data keys.",
        ),
        "zebu" => SetupGuide {
            name: "Zebu",
            fields: const { &[
                key(
                    "User ID and Client ID",
                    "Your Zebu user ID and your app's client ID joined by ::: (userid:::client_id).",
                ),
                secret("Secret Key", "The secret key of your Zebu app."),
            ] },
            banner: "Login with Zebu takes you to Zebu's login page. Log in there and Zebu sends you back to OpenAlgo.",
            redirect_hint: REDIRECT_REGISTER,
        },
        "zerodha" => SetupGuide {
            name: "Zerodha",
            fields: const { &[
                key("API Key", "The API key of your Kite Connect app at developers.kite.trade."),
                secret("API Secret", "The API secret of that app."),
            ] },
            banner: "Login with Zerodha takes you to Zerodha's Kite login page. Log in there and Zerodha sends you back to OpenAlgo. Market data needs an active Kite Connect subscription.",
            redirect_hint: REDIRECT_REGISTER,
        },
        _ => return None,
    })
}

/// Any other broker: the generic fields, and a banner for how it signs in.
fn generic_guide(broker: &str) -> SetupGuide {
    SetupGuide {
        name: "",
        fields: GENERIC_FIELDS,
        banner: match catalog::sign_in(broker) {
            SignIn::Redirect => "Login takes you to your broker's login page. Log in there and your broker sends you back to OpenAlgo.",
            SignIn::Form => "Login opens a short form in OpenAlgo for the details your broker asks for at each login.",
            SignIn::SavedKeys => "Login signs you in with one click using the keys you saved.",
        },
        redirect_hint: REDIRECT_REGISTER,
    }
}

/// The name the broker pages show for `broker`.
pub fn display_name(broker: &str) -> String {
    let g = guide(broker);
    if g.name.is_empty() {
        broker.to_string()
    } else {
        g.name.to_string()
    }
}

/// Whether the broker's keys include the redirect URL (redirect brokers).
pub fn uses_redirect_url(broker: &str) -> bool {
    catalog::sign_in(broker) == SignIn::Redirect
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brokers::catalog::ALL_BROKERS;

    /// The XTS brokers: the only adapters that read the market-data pair.
    const XTS: &[&str] = &[
        "compositedge",
        "fivepaisaxts",
        "ibulls",
        "iifl",
        "jainamxts",
        "rmoney",
        "wisdom",
    ];

    fn has(g: &SetupGuide, k: KeyName) -> bool {
        g.fields.iter().any(|f| f.key == k)
    }

    /// Every catalogue broker has its own entry, with help and fields that
    /// fit how it signs in.
    #[test]
    fn every_broker_has_help_consistent_with_its_sign_in() {
        assert_eq!(ALL_BROKERS.len(), 38);
        let mut names = std::collections::HashSet::new();
        for b in ALL_BROKERS {
            let g = guide(b);
            let kind = catalog::sign_in(b);
            assert!(!g.name.is_empty(), "{b}: no entry of its own");
            assert!(names.insert(g.name), "{b}: name {} is used twice", g.name);
            assert!(!g.banner.is_empty(), "{b}: no banner");
            assert!(
                g.banner.contains(g.name),
                "{b}: the banner should name the broker"
            );
            // Every key a broker needs is named, explained and appears once.
            let mut keys = std::collections::HashSet::new();
            for f in g.fields {
                assert!(
                    !f.label.is_empty() && !f.hint.is_empty(),
                    "{b}: {:?}",
                    f.key
                );
                assert!(keys.insert(f.key), "{b}: {:?} twice", f.key);
                if matches!(f.key, KeyName::ApiSecret | KeyName::ApiSecretMarket) {
                    assert!(f.secret, "{b}: a secret is typed into a hidden input");
                }
            }
            // The API key is always needed, and is the first field.
            assert_eq!(
                g.fields.first().map(|f| f.key),
                Some(KeyName::ApiKey),
                "{b}"
            );
            assert!(g.fields[0].required, "{b}");
            // The client id only where a sign-in is checked against it.
            assert_eq!(
                has(&g, KeyName::ClientId),
                catalog::CLIENT_ID_BROKERS.contains(b),
                "{b}: client id field"
            );
            // The market-data pair only for the XTS brokers, always as a pair.
            assert_eq!(has(&g, KeyName::ApiKeyMarket), XTS.contains(b), "{b}");
            assert_eq!(has(&g, KeyName::ApiSecretMarket), XTS.contains(b), "{b}");
            // The redirect URL, and its hint, only for redirect brokers.
            assert_eq!(uses_redirect_url(b), kind == SignIn::Redirect, "{b}");
            assert_eq!(
                !g.redirect_hint.is_empty(),
                kind == SignIn::Redirect,
                "{b}: redirect hint"
            );
            // The banner says how this kind of sign-in goes.
            let banner = g.banner.to_lowercase();
            match kind {
                SignIn::Redirect => assert!(
                    banner.contains("login page") && banner.contains("back to openalgo"),
                    "{b}: {}",
                    g.banner
                ),
                SignIn::SavedKeys => {
                    assert!(banner.contains("one click"), "{b}: {}", g.banner)
                }
                SignIn::Form => assert!(
                    banner.contains("form") || banner.contains("connect page"),
                    "{b}: {}",
                    g.banner
                ),
            }
        }
    }

    /// The `:::` formats the save checks are spelled out where they apply.
    #[test]
    fn joined_key_formats_are_named_in_the_hint() {
        for (b, format) in [
            ("dhan", "client_id:::api_key"),
            ("flattrade", "client_id:::api_key"),
            ("fivepaisa", "User_Key:::User_ID:::client_id"),
        ] {
            assert!(guide(b).fields[0].hint.contains(format), "{b}");
        }
    }

    #[test]
    fn an_unknown_broker_gets_the_generic_entry() {
        let g = guide("not-a-broker");
        assert_eq!(display_name("not-a-broker"), "not-a-broker");
        assert_eq!(g.fields, GENERIC_FIELDS);
    }
}
