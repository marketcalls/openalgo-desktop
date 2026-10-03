//! Broker catalogue: every web broker id, how it signs in, and for the OAuth
//! brokers how the authorize URL is built and which callback parameter
//! carries the code. Kept apart from the adapters so the login flow can be
//! driven (and tested) without touching adapter code.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthType {
    /// Browser redirect to the broker, back to `/<broker>/callback`.
    OAuth,
    /// In-app form (client id, PIN or password, TOTP) posted to
    /// `/<broker>/callback`.
    Form,
}

/// Every broker the web supports, in its directory order.
pub const ALL_BROKERS: &[&str] = &[
    "aliceblue",
    "angel",
    "arrow",
    "compositedge",
    "definedge",
    "deltaexchange",
    "dhan",
    "dhan_sandbox",
    "firstock",
    "fivepaisa",
    "fivepaisaxts",
    "flattrade",
    "fyers",
    "groww",
    "hdfcsecurities",
    "hdfcsky",
    "ibulls",
    "iifl",
    "iiflcapital",
    "indmoney",
    "jainamxts",
    "kotak",
    "motilal",
    "mstock",
    "nubra",
    "paytm",
    "pocketful",
    "rmoney",
    "samco",
    "shoonya",
    "tradejini",
    "tradesmart",
    "upstox",
    "wisdom",
    "zebu",
    "zerodha",
];

pub fn auth_type(broker: &str) -> AuthType {
    match broker {
        "zerodha" | "fyers" | "upstox" | "dhan" | "arrow" | "paytm" | "pocketful" | "hdfcsky"
        | "hdfcsecurities" | "flattrade" | "compositedge" | "iiflcapital" | "rmoney"
        | "shoonya" | "zebu" | "tradesmart" => AuthType::OAuth,
        _ => AuthType::Form,
    }
}

/// A broker authorize URL and the callback address it was built with. The
/// login flow records `redirect_uri` with the pending sign-in and hands it
/// to the code exchange (`BrokerCredentials::redirect_uri`): Upstox refuses
/// an exchange whose redirect differs from the authorize URL's by a byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizeUrl {
    pub url: String,
    pub redirect_uri: String,
}

/// Whether the broker's redirect carries the `state` OpenAlgo sent. Dhan's
/// consent redirect returns only `tokenId`, so its pending sign-in is
/// matched by broker instead (single use, short expiry).
pub fn callback_carries_state(broker: &str) -> bool {
    !matches!(broker, "dhan")
}

/// Authorize URL with the server-generated `state`, or `None` when the
/// broker has no static OAuth URL (`Broker::begin_login` may build one).
pub fn authorize_url(
    broker: &str,
    api_key: &str,
    redirect_url: &str,
    state: &str,
) -> Option<AuthorizeUrl> {
    let url = authorize_url_string(broker, api_key, redirect_url, state)?;
    Some(AuthorizeUrl {
        url,
        redirect_uri: redirect_url.to_string(),
    })
}

fn authorize_url_string(
    broker: &str,
    api_key: &str,
    redirect_url: &str,
    state: &str,
) -> Option<String> {
    let enc = |s: &str| urlencoding::encode(s).into_owned();
    match broker {
        // Kite returns `redirect_params` verbatim on the callback query.
        "zerodha" => Some(format!(
            "https://kite.zerodha.com/connect/login?v=3&api_key={}&redirect_params={}",
            enc(api_key),
            enc(&format!("state={}", state))
        )),
        "fyers" => Some(format!(
            "https://api-t1.fyers.in/api/v3/generate-authcode?client_id={}&redirect_uri={}&response_type=code&state={}",
            enc(api_key),
            enc(redirect_url),
            enc(state)
        )),
        // The code exchange repeats this redirect byte for byte.
        "upstox" => Some(format!(
            "https://api.upstox.com/v2/login/authorization/dialog?response_type=code&client_id={}&redirect_uri={}&state={}",
            enc(api_key),
            enc(redirect_url),
            enc(state)
        )),
        // Arrow appends the request token to the registered redirect URL.
        // `state` is passed so the callback check can match it; Arrow must
        // echo it back for the sign-in to complete (the web's login URL has
        // no state at all).
        "arrow" => Some(format!(
            "https://app.arrow.trade/app/login?appID={}&state={}",
            enc(api_key),
            enc(state)
        )),
        // OAuth2 code flow; the code exchange repeats this redirect.
        "pocketful" => {
            crate::brokers::pocketful::remember_redirect_uri(redirect_url);
            Some(format!(
                "https://trade.pocketful.in/oauth2/auth?client_id={}&redirect_uri={}&response_type=code&scope=orders%20holdings&state={}",
                enc(api_key),
                enc(redirect_url),
                enc(state)
            ))
        }
        // Paytm Money returns `requestToken` and echoes `state`.
        "paytm" => Some(format!(
            "https://login.paytmmoney.com/merchant-login?apiKey={}&state={}",
            enc(api_key),
            enc(state)
        )),
        // HDFC Sky (web BrokerSelect.tsx builds `/oapi/v1/login?api_key=`).
        // The callback must echo `state` for the sign-in to complete.
        "hdfcsky" => Some(format!(
            "https://developer.hdfcsky.com/oapi/v1/login?api_key={}&state={}",
            enc(api_key),
            enc(state)
        )),
        // InvestRight hosts the login; the callback must echo `state`.
        "hdfcsecurities" => Some(format!(
            "https://developer.hdfcsec.com/oapi/v1/login?api_key={}&state={}",
            enc(api_key),
            enc(state)
        )),
        // XTS third-party login; the session comes back as `session`.
        "compositedge" | "rmoney" => {
            crate::brokers::families::xts::thirdparty_url(broker, api_key, redirect_url, state)
        }
        // Noren family: the authorize URL takes the app key (the half
        // after `:::` of a `userid:::key` entry).
        "shoonya" => Some(crate::brokers::families::noren::authorize_url(
            crate::brokers::shoonya::config(),
            api_key,
            state,
        )),
        "zebu" => Some(crate::brokers::families::noren::authorize_url(
            crate::brokers::zebu::config(),
            api_key,
            state,
        )),
        "tradesmart" => Some(crate::brokers::families::noren::authorize_url(
            crate::brokers::tradesmart::config(),
            api_key,
            state,
        )),
        "flattrade" => Some(crate::brokers::families::noren::authorize_url(
            crate::brokers::flattrade::config(),
            api_key,
            state,
        )),
        // IIFL Capital: appkey + both redirect casings; `state` rides on
        // the callback URL.
        "iiflcapital" => Some(crate::brokers::iiflcapital::auth::login_url(
            api_key,
            redirect_url,
            state,
        )),
        _ => None,
    }
}

/// Which `BrokerCredentials` field a login-form field fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialSlot {
    /// `client_id`: user id, client code, mobile number.
    ClientId,
    /// `password`: password, PIN, MPIN, or a pasted access token.
    Password,
    /// `totp`: TOTP, two-factor code, OTP.
    Totp,
}

/// The credential a form field name fills (web field names).
pub fn credential_slot(name: &str) -> Option<CredentialSlot> {
    match name {
        "userid" | "clientid" | "client_id" | "mobile" | "mobilenumber" => {
            Some(CredentialSlot::ClientId)
        }
        "pin" | "password" | "mpin" | "access_token" => Some(CredentialSlot::Password),
        "totp" | "twofa" | "otp" => Some(CredentialSlot::Totp),
        _ => None,
    }
}

/// One field of a broker's in-app login form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct LoginField {
    /// Form field name posted to `/<broker>/callback`.
    pub name: &'static str,
    /// Label shown to the trader.
    pub label: &'static str,
    /// Masked input; never echoed back.
    pub secret: bool,
    pub required: bool,
}

/// Extra fields a broker's login form shows (beyond the stored API key and
/// secret). Empty for brokers that sign in by redirect or need nothing.
pub fn login_fields(broker: &str) -> &'static [LoginField] {
    match broker {
        // Firstock: user id, password and TOTP typed at each login (the
        // stored API key is the vendor code, the secret the API key).
        "firstock" => &[
            LoginField {
                name: "userid",
                label: "Firstock user id",
                secret: false,
                required: true,
            },
            LoginField {
                name: "password",
                label: "Password",
                secret: true,
                required: true,
            },
            LoginField {
                name: "totp",
                label: "TOTP",
                secret: true,
                required: false,
            },
        ],
        // 5paisa (web BrokerTOTP.tsx): login email, PIN and TOTP typed at
        // each login; the stored API key is `api_key:::user_id:::client_id`
        // and the secret the app's encryption key.
        "fivepaisa" => &[
            LoginField {
                name: "userid",
                label: "5paisa login email",
                secret: false,
                required: true,
            },
            LoginField {
                name: "pin",
                label: "PIN",
                secret: true,
                required: true,
            },
            LoginField {
                name: "totp",
                label: "TOTP from your authenticator app",
                secret: true,
                required: true,
            },
        ],
        // Groww: TOTP for a TOTP API key, or a pasted access token; with
        // neither, the stored API key and secret sign in (approval flow).
        "groww" => &[
            LoginField {
                name: "totp",
                label: "TOTP (if your Groww API key uses TOTP)",
                secret: true,
                required: false,
            },
            LoginField {
                name: "password",
                label: "Access token (if you paste one from Groww)",
                secret: true,
                required: false,
            },
        ],
        // Kotak Neo (web BrokerTOTP.tsx): the stored API key is the UCC and
        // the secret the Neo access token; the form carries the mobile
        // number and TOTP (step one) and the MPIN (step two), posted
        // together as on the web.
        "kotak" => &[
            LoginField {
                name: "mobile",
                label: "Mobile number",
                secret: false,
                required: true,
            },
            LoginField {
                name: "totp",
                label: "TOTP from the Kotak NEO app",
                secret: true,
                required: true,
            },
            LoginField {
                name: "mpin",
                label: "MPIN",
                secret: true,
                required: true,
            },
        ],
        // INDmoney (INDstocks): the stored API key is the Client ID; MPIN and
        // TOTP mint a 24-hour token. Both are optional because a pasted
        // token saved as the API secret signs in without them.
        "indmoney" => &[
            LoginField {
                name: "mpin",
                label: "MPIN",
                secret: true,
                required: false,
            },
            LoginField {
                name: "totp",
                label: "TOTP from your authenticator app",
                secret: true,
                required: false,
            },
        ],
        // Nubra (web authenticate_broker_totp): the stored API key is the
        // registered mobile number and the secret the MPIN; the form
        // carries the TOTP from the authenticator app.
        "nubra" => &[LoginField {
            name: "totp",
            label: "TOTP from your authenticator app",
            secret: true,
            required: true,
        }],
        // Tradejini (web BrokerTOTP form): the CubePlus login PIN goes in
        // `password`, the authenticator code in `twofa`. The stored API key
        // is the individual app's API key.
        "tradejini" => &[
            LoginField {
                name: "password",
                label: "CubePlus login PIN",
                secret: true,
                required: true,
            },
            LoginField {
                name: "twofa",
                label: "TOTP",
                secret: true,
                required: true,
            },
        ],
        // Dhan signs in by consent redirect; a pasted access token (web
        // brlogin `access_token`) is accepted through the form instead.
        "dhan" => &[LoginField {
            name: "access_token",
            label: "Access token (if you paste one from Dhan)",
            secret: true,
            required: true,
        }],
        _ => &[],
    }
}

/// The authorization code on a callback query, per broker.
///
/// Zerodha sends `request_token` (the desktop used to read `code`, which Kite
/// never sends, so Zerodha login could not succeed). Fyers sends `auth_code`
/// and also an unrelated `code=200`, so it must not fall back to `code`.
pub fn extract_code(broker: &str, params: &HashMap<String, String>) -> Option<String> {
    let get = |k: &str| params.get(k).filter(|v| !v.is_empty()).cloned();
    match broker {
        "zerodha" => get("request_token"),
        "fyers" => get("auth_code"),
        // Dhan consent redirect (web brlogin accepts all three spellings).
        "dhan" | "dhan_sandbox" => get("tokenId")
            .or_else(|| get("token_id"))
            .or_else(|| get("token")),
        "compositedge" | "rmoney" => get("session"),
        "paytm" => get("requestToken")
            .or_else(|| get("request_token"))
            .or_else(|| get("code")),
        "arrow" | "hdfcsecurities" | "hdfcsky" => get("request_token")
            .or_else(|| get("requestToken"))
            .or_else(|| get("request-token"))
            .or_else(|| get("code")),
        // IIFL Capital sends `authCode` and `clientId` (web brlogin
        // spellings). The adapter receives one code string, so both travel
        // as `<clientId>:::<authCode>`; without a client id the bare code is
        // passed and the adapter falls back to the stored client id / key.
        "iiflcapital" => {
            let code = get("authCode")
                .or_else(|| get("authcode"))
                .or_else(|| get("auth_code"))
                .or_else(|| get("code"))?;
            let client = get("clientId")
                .or_else(|| get("clientid"))
                .or_else(|| get("client_id"))
                .or_else(|| get("clientCode"))
                .or_else(|| get("clientcode"));
            Some(match client {
                Some(c) => format!(
                    "{}{}{}",
                    c.trim(),
                    crate::brokers::iiflcapital::auth::CODE_SEPARATOR,
                    code.trim()
                ),
                None => code,
            })
        }
        _ => get("code").or_else(|| get("request_token")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn zerodha_uses_request_token() {
        let p = q(&[
            ("status", "success"),
            ("request_token", "rt123"),
            ("action", "login"),
            ("code", "ignored"),
        ]);
        assert_eq!(extract_code("zerodha", &p).as_deref(), Some("rt123"));
        assert_eq!(extract_code("zerodha", &q(&[("code", "x")])), None);
    }

    #[test]
    fn fyers_uses_auth_code_not_status_code() {
        let p = q(&[("s", "ok"), ("code", "200"), ("auth_code", "ac1")]);
        assert_eq!(extract_code("fyers", &p).as_deref(), Some("ac1"));
    }

    #[test]
    fn urls_carry_state() {
        let z = authorize_url(
            "zerodha",
            "kkey",
            "http://127.0.0.1:5000/zerodha/callback",
            "st1",
        )
        .unwrap()
        .url;
        assert!(z.contains("api_key=kkey"));
        assert!(z.contains("redirect_params=state%3Dst1"));
        let f = authorize_url(
            "fyers",
            "APP-100",
            "http://127.0.0.1:5000/fyers/callback",
            "st2",
        )
        .unwrap();
        assert_eq!(f.redirect_uri, "http://127.0.0.1:5000/fyers/callback");
        let f = f.url;
        assert!(f.contains("state=st2"));
        assert!(f.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A5000%2Ffyers%2Fcallback"));
        assert!(authorize_url("angel", "k", "r", "s").is_none());
    }

    #[test]
    fn groww_login_fields_are_optional_secrets() {
        let f = login_fields("groww");
        assert_eq!(f.len(), 2);
        assert_eq!((f[0].name, f[1].name), ("totp", "password"));
        assert!(f.iter().all(|x| x.secret && !x.required));
        assert_eq!(auth_type("groww"), AuthType::Form);
        assert!(login_fields("zerodha").is_empty());
    }

    #[test]
    fn dhan_uses_token_id() {
        let p = q(&[("tokenId", "tid-1"), ("code", "x")]);
        assert_eq!(extract_code("dhan", &p).as_deref(), Some("tid-1"));
        assert_eq!(
            extract_code("dhan", &q(&[("token_id", "t2")])).as_deref(),
            Some("t2")
        );
        assert_eq!(extract_code("dhan", &q(&[("code", "x")])), None);
        assert_eq!(auth_type("kotak"), AuthType::Form);
        assert_eq!(auth_type("dhan_sandbox"), AuthType::Form);
        // The sandbox signs in with the stored token alone.
        assert!(login_fields("dhan_sandbox").is_empty());
        assert_eq!(login_fields("dhan")[0].name, "access_token");
        assert_eq!(
            credential_slot("access_token"),
            Some(CredentialSlot::Password)
        );
        assert_eq!(credential_slot("mobile"), Some(CredentialSlot::ClientId));
        assert_eq!(credential_slot("otp"), Some(CredentialSlot::Totp));
        assert!(!callback_carries_state("dhan"));
        assert!(callback_carries_state("upstox"));
    }

    #[test]
    fn kotak_login_fields_are_mobile_totp_mpin() {
        let f = login_fields("kotak");
        let names: Vec<&str> = f.iter().map(|x| x.name).collect();
        assert_eq!(names, ["mobile", "totp", "mpin"]);
        assert!(f.iter().all(|x| x.required));
        assert!(!f[0].secret && f[1].secret && f[2].secret);
    }

    #[test]
    fn noren_family_sign_in() {
        for b in ["shoonya", "zebu", "tradesmart", "flattrade"] {
            assert_eq!(auth_type(b), AuthType::OAuth, "{}", b);
            let u = authorize_url(b, "U1:::APPKEY", "r", "st9").unwrap().url;
            assert!(u.contains("APPKEY") && u.ends_with("state=st9"), "{}", u);
            assert!(!u.contains("U1"));
        }
        assert!(authorize_url("zebu", "k", "r", "s")
            .unwrap()
            .url
            .starts_with("https://go.mynt.in/OAuthlogin/authorize/oauth?client_id=k"));
        assert_eq!(auth_type("firstock"), AuthType::Form);
        let f = login_fields("firstock");
        assert_eq!(
            f.iter().map(|x| x.name).collect::<Vec<_>>(),
            ["userid", "password", "totp"]
        );
        assert!(f[1].secret && !f[0].secret);
        let p = q(&[("code", "c1"), ("state", "s")]);
        assert_eq!(extract_code("shoonya", &p).as_deref(), Some("c1"));
    }

    #[test]
    fn arrow_login_url_and_request_token() {
        assert_eq!(auth_type("arrow"), AuthType::OAuth);
        let u = authorize_url("arrow", "APP 1", "r", "st5").unwrap();
        assert_eq!(
            u,
            "https://app.arrow.trade/app/login?appID=APP%201&state=st5"
        );
        // Arrow sends `request-token` (hyphen); the other spellings are
        // fallbacks the web also accepts.
        let p = q(&[("request-token", "rt9"), ("checksum", "x")]);
        assert_eq!(extract_code("arrow", &p).as_deref(), Some("rt9"));
        assert_eq!(
            extract_code("arrow", &q(&[("code", "c2")])).as_deref(),
            Some("c2")
        );
        assert!(login_fields("arrow").is_empty());
    }

    #[test]
    fn pocketful_oauth2_url_and_code() {
        let u = authorize_url(
            "pocketful",
            "cid-1",
            "http://127.0.0.1:5000/pocketful/callback",
            "st7",
        )
        .unwrap();
        assert!(u.starts_with("https://trade.pocketful.in/oauth2/auth?client_id=cid-1&"));
        assert!(u.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A5000%2Fpocketful%2Fcallback"));
        assert!(u.contains("response_type=code&scope=orders%20holdings&state=st7"));
        assert_eq!(
            crate::brokers::pocketful::redirect_uri(),
            "http://127.0.0.1:5000/pocketful/callback"
        );
        assert_eq!(auth_type("pocketful"), AuthType::OAuth);
        let p = q(&[("code", "c9"), ("state", "st7")]);
        assert_eq!(extract_code("pocketful", &p).as_deref(), Some("c9"));
    }

    #[test]
    fn paytm_sign_in() {
        assert_eq!(auth_type("paytm"), AuthType::OAuth);
        let u = authorize_url(
            "paytm",
            "pk 1",
            "http://127.0.0.1:5000/paytm/callback",
            "st7",
        )
        .unwrap();
        assert_eq!(
            u,
            "https://login.paytmmoney.com/merchant-login?apiKey=pk%201&state=st7"
        );
        let p = q(&[("requestToken", "rt9"), ("state", "st7"), ("code", "x")]);
        assert_eq!(extract_code("paytm", &p).as_deref(), Some("rt9"));
        assert_eq!(
            extract_code("paytm", &q(&[("request_token", "rt2")])).as_deref(),
            Some("rt2")
        );
        assert_eq!(
            extract_code("paytm", &q(&[("code", "c3")])).as_deref(),
            Some("c3")
        );
        assert_eq!(extract_code("paytm", &q(&[("state", "s")])), None);
        assert!(login_fields("paytm").is_empty());
    }

    #[test]
    fn hdfcsky_login_url_and_callback_token() {
        assert_eq!(auth_type("hdfcsky"), AuthType::OAuth);
        let u = authorize_url("hdfcsky", "key 1", "r", "st7").unwrap();
        assert_eq!(
            u,
            "https://developer.hdfcsky.com/oapi/v1/login?api_key=key%201&state=st7"
        );
        for k in ["request_token", "requestToken", "request-token", "code"] {
            let p = q(&[(k, "rt9"), ("state", "st7")]);
            assert_eq!(extract_code("hdfcsky", &p).as_deref(), Some("rt9"), "{}", k);
        }
        assert!(login_fields("hdfcsky").is_empty());
    }

    #[test]
    fn hdfcsecurities_login_url_and_code() {
        assert_eq!(auth_type("hdfcsecurities"), AuthType::OAuth);
        let u = authorize_url("hdfcsecurities", "key 1", "r", "st5").unwrap();
        assert_eq!(
            u,
            "https://developer.hdfcsec.com/oapi/v1/login?api_key=key%201&state=st5"
        );
        for k in ["request_token", "requestToken", "request-token", "code"] {
            let p = q(&[(k, "rt9"), ("state", "st5")]);
            assert_eq!(extract_code("hdfcsecurities", &p).as_deref(), Some("rt9"));
        }
        assert!(login_fields("hdfcsecurities").is_empty());
    }

    #[test]
    fn fivepaisa_login_fields_are_email_pin_totp() {
        assert_eq!(auth_type("fivepaisa"), AuthType::Form);
        let f = login_fields("fivepaisa");
        let names: Vec<&str> = f.iter().map(|x| x.name).collect();
        assert_eq!(names, ["userid", "pin", "totp"]);
        assert!(f.iter().all(|x| x.required));
        assert!(!f[0].secret && f[1].secret && f[2].secret);
    }

    #[test]
    fn indmoney_login_fields_are_mpin_and_totp() {
        assert_eq!(auth_type("indmoney"), AuthType::Form);
        let f = login_fields("indmoney");
        assert_eq!(
            f.iter().map(|x| x.name).collect::<Vec<_>>(),
            ["mpin", "totp"]
        );
        assert!(f.iter().all(|x| x.secret && !x.required));
    }

    #[test]
    fn nubra_signs_in_with_a_totp_form() {
        assert_eq!(auth_type("nubra"), AuthType::Form);
        let f = login_fields("nubra");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].name, "totp");
        assert!(f[0].secret && f[0].required);
        assert!(authorize_url("nubra", "k", "r", "s").is_none());
    }

    #[test]
    fn tradejini_login_fields_are_pin_and_totp() {
        assert_eq!(auth_type("tradejini"), AuthType::Form);
        let f = login_fields("tradejini");
        assert_eq!(
            f.iter().map(|x| x.name).collect::<Vec<_>>(),
            ["password", "twofa"]
        );
        assert!(f.iter().all(|x| x.secret && x.required));
        assert!(authorize_url("tradejini", "k", "r", "s").is_none());
    }

    #[test]
    fn iiflcapital_sign_in() {
        assert_eq!(auth_type("iiflcapital"), AuthType::OAuth);
        let u = authorize_url(
            "iiflcapital",
            "CL1:::APPKEY",
            "http://127.0.0.1:5000/iiflcapital/callback",
            "st7",
        )
        .unwrap();
        assert_eq!(
            u,
            "https://markets.iiflcapital.com/?v=1&appkey=APPKEY&redirecturl=http://127.0.0.1:5000/iiflcapital/callback?state=st7&redirectUrl=http://127.0.0.1:5000/iiflcapital/callback?state=st7"
        );
        let p = q(&[("authCode", "ac9"), ("clientId", "778"), ("state", "s")]);
        assert_eq!(
            extract_code("iiflcapital", &p).as_deref(),
            Some("778:::ac9")
        );
        let p = q(&[("authcode", "ac9"), ("clientcode", "779")]);
        assert_eq!(
            extract_code("iiflcapital", &p).as_deref(),
            Some("779:::ac9")
        );
        let p = q(&[("code", "ac1")]);
        assert_eq!(extract_code("iiflcapital", &p).as_deref(), Some("ac1"));
        assert_eq!(extract_code("iiflcapital", &q(&[("clientId", "1")])), None);
        assert!(login_fields("iiflcapital").is_empty());
    }

    #[test]
    fn catalogue_has_all_web_brokers() {
        assert_eq!(ALL_BROKERS.len(), 36);
        assert_eq!(auth_type("angel"), AuthType::Form);
        assert_eq!(auth_type("zerodha"), AuthType::OAuth);
    }
}
