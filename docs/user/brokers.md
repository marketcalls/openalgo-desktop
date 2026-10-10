# Broker sign-in

Every broker is set up the same way:

1. Create an API app on your broker's developer portal. Where it asks for a
   redirect URL, enter `http://127.0.0.1:5000/<broker>/callback` (for example
   `http://127.0.0.1:5000/upstox/callback`). If you changed the app port in
   Server Settings, use that port instead of 5000.
2. In OpenAlgo Desktop open **Profile**, then the **Broker** tab. Under
   **Update Credentials** choose your broker, enter the **Broker API Key** and
   **Broker API Secret**, and any other field the form shows for your broker.
   Choose **Save Broker Credentials**.
3. Open the **Broker** page, pick the broker and choose **Connect Account**.

What happens at step 3 depends on how your broker signs in. Brokers fall into
three groups.

## Brokers that sign in on their own website

Zerodha, Fyers, Upstox, Dhan, Arrow, Paytm Money, Pocketful, HDFC Sky, HDFC
Securities, IIFL Capital, Alice Blue, CompositEdge, RMoney, Flattrade,
Shoonya, Zebu and TradeSmart.

**Connect Account** takes you to the broker's login page. Log in there,
approve access, and the broker sends you back to OpenAlgo, which finishes the
sign-in and opens the dashboard.

- The redirect URL in your broker app must match the one on the Broker tab
  exactly. Upstox in particular refuses a sign-in whose redirect URL differs
  by a single character, including `localhost` in place of `127.0.0.1`.
- Always start from **Connect Account** in OpenAlgo. A sign-in that OpenAlgo
  did not start, or one that has expired, is refused with "This broker sign-in
  was not started from OpenAlgo or has expired". Choose Connect Account again.
- Finish the broker's login within a few minutes of starting it. Some brokers
  (Dhan, Flattrade, Shoonya, Zebu, TradeSmart, Arrow, HDFC Sky, HDFC
  Securities and Alice Blue) do not send back OpenAlgo's sign-in reference,
  so for them OpenAlgo accepts only the most recent sign-in started from the
  same window, within three minutes.
- Zerodha needs an active Kite Connect subscription, and Dhan an active Data
  API subscription, for market data.

## Brokers bound to your client ID

For some of the brokers above OpenAlgo checks that the account you log in with
is the account you set up, so that nobody can slip their own session into your
app. These brokers need your client ID:

| Broker | Where the client ID goes |
| --- | --- |
| Dhan | In the API key, as `client_id:::api_key` (the Profile form shows this format) |
| Flattrade | In the API key, as `client_id:::api_key` |
| Shoonya, Zebu, TradeSmart | In the API key as `client_id:::api_key`, or in the Client ID field |
| Arrow, HDFC Sky, HDFC Securities, Alice Blue | The **Client ID** field the Profile form shows for these brokers |

If the client ID is missing, the sign-in stops with "Add your client id in
Profile, Broker Configuration". If you log in to the broker with a different
account, OpenAlgo refuses it and changes nothing: "This sign-in is for a
different account than the one set up in OpenAlgo". To switch to another
account at the same broker, log out of the broker, save the broker settings
again with the new account's details, then connect.

## Brokers that sign in with a form in the app

**Connect Account** opens a login form inside OpenAlgo. Type the details your
broker asks for each time you connect.

| Broker | What the form asks for |
| --- | --- |
| Angel One | Client ID, PIN, TOTP |
| Kotak Securities | Mobile number, TOTP from the Kotak NEO app, MPIN |
| 5 Paisa | Login email or client ID, PIN, TOTP |
| Firstock | User ID, password, TOTP |
| Motilal Oswal | User ID, password, date of birth, TOTP |
| mStock by Mirae Asset | Password, TOTP |
| Tradejini | CubePlus login PIN, TOTP |
| Nubra | TOTP |
| IndMoney | MPIN and TOTP |
| Definedge | The OTP that Definedge sends to your registered mobile or email when the form opens |
| Groww | TOTP if your Groww API key uses TOTP, or an access token you paste |

Notes:

- **TOTP** is the 6-digit code from your authenticator app for that broker
  account. Codes change every 30 seconds; if a sign-in fails, wait for the
  next code. Check that your computer's clock is set automatically.
- **5 Paisa**: the API key is entered as `User_Key:::User_ID:::client_id`, as
  the Profile form shows.
- **IndMoney**: the API key is the Client ID shown at indstocks.com, API
  Trading, Access Tokens. Leave the API secret empty to log in with MPIN and
  TOTP.
- **Samco** signs in with the saved API key and secret on its own **Connect**
  page, which also checks that your computer's internet address matches the
  static IP registered with Samco.

## Brokers that sign in with saved keys

The Symphony XTS brokers (5 Paisa (XTS), JainamXts, Ibulls, IIFL, Wisdom
Capital), Delta Exchange and Dhan (Sandbox) sign in with the keys you saved.
For the XTS brokers, also fill in the **Market API Key** and **Market API
Secret** under **Market Data API (Optional)** on the Broker tab; they are the
market data keys from the same XTS portal.

Delta Exchange is a crypto exchange: its symbols use the `CRYPTO` exchange.

## Daily sign-in

Indian brokers end API sessions every night. OpenAlgo Desktop resumes your
broker session when you sign in to the app during the same trading day, and
ends it at about 03:00 IST, after which you connect again from the Broker
page. Logging out of OpenAlgo also ends the broker session.

## Switching brokers

You can save credentials for several brokers. To switch, choose the other
broker on the Broker tab in Profile and save, or pick it on the Broker page,
then connect. While a broker session is live, OpenAlgo asks you to confirm
first: the switch ends that session in OpenAlgo (stop-loss and target exits
OpenAlgo places for it stop, and your trading platforms can no longer trade
through it), while your open positions and orders stay at the broker. If the
switch cannot be saved, nothing changes and the live session keeps running.
No restart is needed.

Sign-in failing? See [Troubleshooting](troubleshooting.md#broker-sign-in-fails).
