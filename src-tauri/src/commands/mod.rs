//! Tauri commands: only what the shell alone can do. Everything else is HTTP.
//!
//! | Command          | Who may call          | Why                                         |
//! |------------------|-----------------------|---------------------------------------------|
//! | `startup_status` | anyone                | the start-up page shows why the server is not running (no secrets) |
//! | `retry_server`   | anyone, only while the server is NOT running | recover from a taken port before any page can load |
//! | `restart_server` | signed-in user        | apply new port / LAN settings               |
//! | `open_external`  | signed-in user        | open an http(s) link in the system browser  |
//! | `reset_account`  | the main window on the app's own page, after a native confirmation | start over with a new account |
//! | `start_fresh`    | the main window on the app's own page, after a native confirmation | a new password without the old one: forgotten, or an earlier build's keys unreadable (KEY-01) |
//! | `window_proof`   | the main window on the app's own page | the proof its sign-in carries, so only the app window moves an earlier build's keys and hears why keys could not be opened (KEY-01) |
//! | `restart_to_update` | signed-in user, the main window on the app's own page | install the downloaded, verified update and restart (`services::update` decides; it asks again if new activity appeared) |

pub mod quit;

use crate::error::{AppError, Result};
use crate::server::{self, ServerHandle};
use crate::services::lifecycle::quit::QuitGate;
use crate::services::update::{Activity, Decision, Warning};
use crate::state::{AppState, ServerStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};

pub struct ShellState {
    pub ctx: Arc<AppState>,
    pub server: tokio::sync::Mutex<Option<ServerHandle>>,
    /// Everything was stopped (`stop_app` ran): on exit, or before the
    /// Windows installer replaces the app.
    pub stopped: AtomicBool,
    /// Closing the main window asks first while work runs (ARCH-03).
    pub quit: QuitGate,
    /// The quit check waiting for the trader's answer.
    quit_check: parking_lot::Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

impl ShellState {
    pub fn new(ctx: Arc<AppState>, server: Option<ServerHandle>) -> Self {
        Self {
            ctx,
            server: tokio::sync::Mutex::new(server),
            stopped: AtomicBool::new(false),
            quit: QuitGate::default(),
            quit_check: parking_lot::Mutex::new(None),
        }
    }
}

/// Stop the market data feed, the server and every service, once. Runs on
/// exit, and before the Windows update installer replaces the app.
pub async fn stop_app(handle: &AppHandle) {
    let Some(shell) = handle.try_state::<ShellState>() else {
        return;
    };
    if shell.stopped.swap(true, Ordering::SeqCst) {
        return;
    }
    quit::stop(&shell);
    if let Some(f) = handle.try_state::<Arc<crate::feed::FeedService>>() {
        f.stop().await;
    }
    if let Some(h) = shell.server.lock().await.take() {
        h.stop().await;
    }
    shell.ctx.shutdown().await;
}

/// The guard every non-bootstrap command calls first.
pub fn require_user(ctx: &AppState) -> Result<String> {
    ctx.signed_in_user()
        .ok_or_else(|| AppError::Auth("Sign in to OpenAlgo first.".into()))
}

/// Permissions of the app's own pages served by the local server: window
/// basics, opening http(s) links, and the account reset. Granted at run
/// time to the exact origin the server listens on, never to every port on
/// loopback (security review S-08).
pub const REMOTE_APP_PERMISSIONS: [&str; 11] = [
    "core:app:allow-version",
    "core:window:allow-set-title",
    "core:window:allow-minimize",
    "core:window:allow-maximize",
    "core:window:allow-unmaximize",
    "core:window:allow-set-focus",
    "shell:allow-open",
    "allow-reset-account",
    "allow-start-fresh",
    "allow-window-proof",
    "allow-restart-to-update",
];

/// The page addresses trusted with [`REMOTE_APP_PERMISSIONS`]: the server's
/// listening port on loopback, plus the Vite dev server in development.
pub fn remote_app_urls(port: u16, development: bool) -> Vec<String> {
    let mut v = vec![
        format!("http://127.0.0.1:{}/*", port),
        format!("http://localhost:{}/*", port),
    ];
    if development {
        v.push("http://localhost:5173/*".into());
    }
    v
}

/// Whether `url` is the app's own page: exactly `127.0.0.1` or `localhost`
/// (any case) on the live server port, which must be known (`live` is 0
/// before the first bind, after a failed bind and once the listener stops,
/// and then nothing is the app's page). In development the Vite dev server
/// page, `localhost:5173`, is too. Any other address on this computer
/// (another port, `127.0.0.2`, `[::1]`) is not.
pub fn app_page(url: &url::Url, live: u16, development: bool) -> bool {
    if url.scheme() != "http" {
        return false;
    }
    let host = url.host_str().map(str::to_ascii_lowercase);
    let port = url.port_or_known_default();
    if development && host.as_deref() == Some("localhost") && port == Some(5173) {
        return true;
    }
    live != 0
        && port == Some(live)
        && matches!(host.as_deref(), Some("127.0.0.1") | Some("localhost"))
}

/// Whether the main window may load `url` while the server is bound to
/// `live` (0: not bound). Tauri cannot take back a capability granted at
/// run time, so after the server moves from port A to port B the grant for
/// A stays registered; what keeps it unusable is that the main window loads
/// no page on this computer but the app's own ([`app_page`]), and nothing on
/// this computer at all while no listener holds the port. Pages that are not
/// on this computer (a broker's sign-in page, which must open in this window
/// so its redirect lands on the same browser session) and the bundled
/// start-up page (not http) get none of the app's permissions and stay
/// reachable. The app's own commands re-check the caller's page too
/// ([`reset_caller_allowed`]).
pub fn main_window_may_load(url: &url::Url, live: u16, development: bool) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return true;
    }
    let on_this_computer = match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback() || ip.is_unspecified(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback() || ip.is_unspecified(),
        None => true,
    };
    !on_this_computer || app_page(url, live, development)
}

/// Grant the main window's pages on `port` their permissions. Called when
/// the server starts and again when it moves to another port, before the
/// window is sent there.
pub fn trust_app_origin(app: &AppHandle, port: u16) {
    let mut cap = tauri::ipc::CapabilityBuilder::new(format!("remote-app-{}", port))
        .window("main")
        .local(false);
    for url in remote_app_urls(port, cfg!(debug_assertions)) {
        cap = cap.remote(url);
    }
    for p in REMOTE_APP_PERMISSIONS {
        cap = cap.permission(p);
    }
    if let Err(e) = app.add_capability(cap) {
        tracing::error!("Could not grant the app window its permissions: {}", e);
    }
}

/// URL the main window should show for the current server state.
pub fn window_url(ctx: &AppState) -> Option<url::Url> {
    match &*ctx.server_status.read() {
        ServerStatus::Running { port, .. } => {
            url::Url::parse(&format!("http://127.0.0.1:{}/", port)).ok()
        }
        _ => None,
    }
}

fn navigate(app: &AppHandle, ctx: &AppState) {
    if cfg!(debug_assertions) {
        // Development: the window stays on the Vite dev server.
        return;
    }
    if let (Some(w), Some(u)) = (app.get_webview_window("main"), window_url(ctx)) {
        if let Err(e) = w.navigate(u) {
            tracing::error!("Could not reload the window: {}", e);
        }
    }
}

/// The HTTP listener's state (the start-up page reads it as before) plus
/// the market data listener's under `ws`, and, when the data folder was
/// not opened, why (`data_refused`).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct StartupStatus {
    #[serde(flatten)]
    pub http: ServerStatus,
    pub ws: ServerStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_refused: Option<String>,
}

pub fn startup_status_of(ctx: &AppState) -> StartupStatus {
    StartupStatus {
        http: ctx.server_status.read().clone(),
        ws: ctx.feed_status.read().clone(),
        data_refused: None,
    }
}

/// The data folder was not opened (REL-02); the start-up page shows this
/// message and nothing else runs.
pub struct DataRefusal(pub String);

/// The running app's lock on its data folder, held until it exits.
pub struct DataFolderHold(pub Option<crate::db::upgrade::DataFolderLock>);

/// What the start-up page shows when the data folder was refused.
pub fn data_refused_status(message: &str) -> StartupStatus {
    let failed = ServerStatus::Failed {
        message: message.to_string(),
    };
    StartupStatus {
        http: failed,
        ws: ServerStatus::Starting,
        data_refused: Some(message.to_string()),
    }
}

#[tauri::command]
pub fn startup_status(app: AppHandle) -> StartupStatus {
    if let Some(r) = app.try_state::<DataRefusal>() {
        return data_refused_status(&r.0);
    }
    match app.try_state::<ShellState>() {
        Some(shell) => startup_status_of(&shell.ctx),
        None => StartupStatus {
            http: ServerStatus::Starting,
            ws: ServerStatus::Starting,
            data_refused: None,
        },
    }
}

#[tauri::command]
pub async fn retry_server(
    app: AppHandle,
    shell: State<'_, ShellState>,
    port: Option<u16>,
) -> Result<ServerStatus> {
    let mut guard = shell.server.lock().await;
    if guard.is_some() {
        return Err(AppError::Validation("OpenAlgo is already running.".into()));
    }
    if let Some(p) = port {
        crate::services::settings_service::save_server(
            &shell.ctx,
            &crate::config::ServerConfigUpdate {
                http_port: Some(p),
                ..Default::default()
            },
        )?;
        shell.ctx.reload_config()?;
    }
    match server::start(shell.ctx.clone()).await {
        Ok(h) => {
            trust_app_origin(&app, h.addr.port());
            *guard = Some(h);
            navigate(&app, &shell.ctx);
        }
        Err(st) => return Ok(st),
    }
    Ok(shell.ctx.server_status.read().clone())
}

#[tauri::command]
pub async fn restart_server(app: AppHandle, shell: State<'_, ShellState>) -> Result<ServerStatus> {
    require_user(&shell.ctx)?;
    shell.ctx.reload_config()?;
    let mut guard = shell.server.lock().await;
    if let Some(h) = guard.take() {
        h.stop().await;
    }
    match server::start(shell.ctx.clone()).await {
        Ok(h) => {
            trust_app_origin(&app, h.addr.port());
            *guard = Some(h);
            navigate(&app, &shell.ctx);
            Ok(shell.ctx.server_status.read().clone())
        }
        Err(st) => {
            if let Some(w) = app.get_webview_window("main") {
                if let Ok(u) = url::Url::parse("tauri://localhost/index.html") {
                    let _ = w.navigate(u);
                }
            }
            Ok(st)
        }
    }
}

#[tauri::command]
pub fn open_external(app: AppHandle, shell: State<'_, ShellState>, url: String) -> Result<()> {
    require_user(&shell.ctx)?;
    let parsed = url::Url::parse(&url)
        .map_err(|_| AppError::Validation("That link is not a web address.".into()))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(AppError::Validation("Only web links can be opened.".into()));
    }
    #[allow(deprecated)]
    tauri_plugin_shell::ShellExt::shell(&app)
        .open(parsed.as_str(), None)
        .map_err(|e| AppError::Internal(e.to_string()))
}

/// Who may start an account reset: the main window, showing the app's own
/// page ([`app_page`]: the local server on the port it is bound to now, or
/// the Vite dev server in development builds). A hidden runner window, a
/// broker page the main window was sent to, a page on a port the server has
/// left, or anything while no listener is bound (`live_port` 0) is refused.
pub fn reset_caller_allowed(label: &str, url: &url::Url, live_port: u16) -> bool {
    label == "main" && app_page(url, live_port, cfg!(debug_assertions))
}

/// Refuse a call that does not come from the main window on the app's own
/// page ([`reset_caller_allowed`]): `what` names it in the log, `refusal` is
/// the trader's answer.
fn require_app_window(
    window: &tauri::WebviewWindow,
    ctx: &AppState,
    what: &str,
    refusal: &str,
) -> Result<()> {
    let url = window
        .url()
        .map_err(|e| AppError::Internal(format!("window address: {}", e)))?;
    if reset_caller_allowed(window.label(), &url, ctx.live_port()) {
        return Ok(());
    }
    tracing::warn!("{} refused: not requested from the OpenAlgo window", what);
    Err(AppError::Auth(refusal.into()))
}

/// A question the trader answers in a native dialog, which page scripts
/// cannot click.
struct Question<'a> {
    title: &'a str,
    message: &'a str,
    yes: &'a str,
    no: &'a str,
}

/// Whether the trader chose `yes` (closing the dialog is a no).
async fn confirmed(app: &AppHandle, window: &tauri::WebviewWindow, q: Question<'_>) -> bool {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .message(q.message)
        .title(q.title)
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            q.yes.into(),
            q.no.into(),
        ))
        .parent(window)
        .show(move |yes| {
            let _ = tx.send(yes);
        });
    rx.await.unwrap_or(false)
}

/// The answer the reset page gets back.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ResetOutcome {
    /// `reset` or `cancelled`.
    pub status: &'static str,
    pub message: String,
}

/// Last-resort recovery when the password and the authenticator are both
/// lost. Only the person at this computer can do it: the call must come
/// from the main window on the app's own page, and the trader confirms in a
/// native dialog that page scripts cannot click. No HTTP route resets the
/// account, so nothing reaching the server over the network (a LAN device,
/// a tunnel, a web page in a browser) can.
#[tauri::command]
pub async fn reset_account(
    app: AppHandle,
    window: tauri::WebviewWindow,
    shell: State<'_, ShellState>,
) -> Result<ResetOutcome> {
    require_app_window(
        &window,
        &shell.ctx,
        "Account reset",
        "Reset account works only from the OpenAlgo Desktop window on this computer.",
    )?;
    let question = Question {
        title: "Reset your OpenAlgo account?",
        message: "This removes your OpenAlgo login, API key and saved broker keys, ends your \
broker session, stops every AI client (MCP) token, unlinks Telegram and WhatsApp, and gives \
your strategy and Chartink webhooks new addresses. Your trade and strategy history stays. You \
then create a new account.",
        yes: "Reset account",
        no: "Keep my account",
    };
    if !confirmed(&app, &window, question).await {
        return Ok(ResetOutcome {
            status: "cancelled",
            message: "Your account was not changed.".into(),
        });
    }
    crate::services::auth_service::AuthService::reset_account_everywhere(&shell.ctx).await?;
    Ok(ResetOutcome {
        status: "reset",
        message: "Your account was removed. Create a new account to continue.".into(),
    })
}

/// The app window's proof (KEY-01 security review): the sign-in page of
/// the main window sends it with the password, and the server then treats
/// the sign-in as the app window's (`security::window`). Only the main
/// window on the app's own page gets it; a browser tab cannot call this at
/// all. A new one every start, never stored.
#[tauri::command]
pub fn window_proof(window: tauri::WebviewWindow, shell: State<'_, ShellState>) -> Result<String> {
    require_app_window(
        &window,
        &shell.ctx,
        "Window proof",
        "This works only in the OpenAlgo Desktop window on this computer.",
    )?;
    Ok(shell
        .ctx
        .security
        .window_proof()
        .token()
        .expose()
        .to_string())
}

/// The answer the new-password form gets back.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct StartFreshOutcome {
    /// `done` or `cancelled`.
    pub status: &'static str,
    pub message: String,
}

/// What the trader confirms before a fresh start: for a forgotten password
/// (`forgot`) or for an earlier build's keys that cannot be read.
fn start_fresh_question(forgot: bool) -> Question<'static> {
    Question {
        title: if forgot {
            "Set a new password?"
        } else {
            "Start fresh with a new password?"
        },
        message: "Your account, settings, strategies and history stay. Your saved broker keys, \
your API key and two-factor login cannot be opened without the old password, so they are \
removed: afterwards you enter your broker keys again, copy your new API key into the programs \
that use it, and set up two-factor login again. Telegram and WhatsApp are set up again too.",
        yes: if forgot {
            "Set new password"
        } else {
            "Start fresh"
        },
        no: "Keep everything as it is",
    }
}

/// A new password without the old one (KEY-01): keeps the account, its
/// settings and history, and drops what the old keys protected (saved
/// broker keys, the API key, two-factor login), for a forgotten password or
/// an earlier build's keychain that cannot be read. Only the person at this
/// computer can do it: the call must come from the main window on the
/// app's own page, and the trader confirms in a native dialog page scripts
/// cannot click. No HTTP route reaches it, so nothing over the network (a
/// LAN device, a tunnel, a web page) can replace the password.
#[tauri::command]
pub async fn start_fresh(
    app: AppHandle,
    window: tauri::WebviewWindow,
    shell: State<'_, ShellState>,
    new_password: String,
    confirm_password: String,
    reason: String,
) -> Result<StartFreshOutcome> {
    use crate::services::auth_service::AuthService;
    require_app_window(
        &window,
        &shell.ctx,
        "Fresh start",
        "A new password can be set only from the OpenAlgo Desktop window on this computer.",
    )?;
    let new = crate::security::Secret::new(new_password);
    let confirm = crate::security::Secret::new(confirm_password);
    // Checked before the dialog, so the trader is not asked to confirm a
    // password that would be refused.
    AuthService::check_new_password(new.expose(), confirm.expose())?;
    if !confirmed(&app, &window, start_fresh_question(reason == "forgot")).await {
        return Ok(StartFreshOutcome {
            status: "cancelled",
            message: "Nothing was changed.".into(),
        });
    }
    AuthService::start_fresh_everywhere(&shell.ctx, new.expose(), confirm.expose()).await?;
    tracing::info!("New password set from the desktop window");
    Ok(StartFreshOutcome {
        status: "done",
        message: "Your new password is set. Sign in with it, then enter your broker keys again \
on Broker Configuration and copy your new API key from the API Key page."
            .into(),
    })
}

/// Every shell command a window may call; each is reachable only from a
/// capability that grants its permission (`build.rs`, `default.json`,
/// [`REMOTE_APP_PERMISSIONS`]).
pub fn handler() -> impl Fn(tauri::ipc::Invoke<tauri::Wry>) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        startup_status,
        retry_server,
        restart_server,
        open_external,
        reset_account,
        start_fresh,
        window_proof,
        restart_to_update,
    ]
}

/// The answer the update dialog gets back from `restart_to_update`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RestartOutcome {
    /// `restarting`, `confirm` (something new runs: show `warning`, send
    /// `activity` back to go ahead), `refused` or `failed`.
    pub status: &'static str,
    pub message: Option<String>,
    pub activity: Option<Activity>,
    pub warning: Option<Warning>,
}

impl RestartOutcome {
    fn of(status: &'static str, message: Option<String>) -> Self {
        Self {
            status,
            message,
            activity: None,
            warning: None,
        }
    }
}

/// Install the downloaded, verified update and restart into it. Only the
/// trader in the desktop window can: the window must be the main one on
/// the app's own page, and the update service goes ahead only while
/// everything running is within what the trader confirmed (it answers
/// `confirm` again otherwise). On exit the feed, the server and every
/// service stop as on any exit.
#[tauri::command]
pub async fn restart_to_update(
    app: AppHandle,
    window: tauri::WebviewWindow,
    shell: State<'_, ShellState>,
    confirmed: Option<Activity>,
) -> Result<RestartOutcome> {
    require_user(&shell.ctx)?;
    require_app_window(
        &window,
        &shell.ctx,
        "Update restart",
        "Updates are installed only from the OpenAlgo Desktop window on this computer.",
    )?;
    let Some(backend) = app.try_state::<Arc<crate::updater::PluginBackend<tauri::Wry>>>() else {
        let m = "This copy of OpenAlgo cannot install updates.";
        return Ok(RestartOutcome::of("refused", Some(m.into())));
    };
    let ctx = shell.ctx.clone();
    match ctx.updates.begin_restart(&ctx, confirmed).await {
        Decision::Proceed(_) => {}
        Decision::Confirm { activity, warning } => {
            return Ok(RestartOutcome {
                status: "confirm",
                message: None,
                activity: Some(activity),
                warning: Some(warning),
            })
        }
        Decision::Refused(m) => return Ok(RestartOutcome::of("refused", Some(m))),
    }
    Ok(
        match crate::updater::install_and_restart(&app, &backend, &ctx).await {
            Ok(()) => RestartOutcome::of("restarting", None),
            Err(m) => RestartOutcome::of("failed", Some(m)),
        },
    )
}

#[cfg(test)]
mod reset_tests {
    use super::{remote_app_urls, reset_caller_allowed};

    /// S-08: the app's permissions go to the server's own port only; no
    /// capability file trusts every port on loopback.
    #[test]
    fn app_permissions_are_scoped_to_the_listening_port() {
        assert_eq!(
            remote_app_urls(5000, false),
            vec!["http://127.0.0.1:5000/*", "http://localhost:5000/*"]
        );
        assert!(remote_app_urls(5500, true).contains(&"http://localhost:5173/*".to_string()));
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities");
        for f in std::fs::read_dir(dir).unwrap() {
            let text = std::fs::read_to_string(f.unwrap().path()).unwrap();
            assert!(
                !text.contains(":*\""),
                "a capability trusts every port: {}",
                text
            );
            assert!(
                !text.contains("\"remote\""),
                "remote pages are granted at run time"
            );
        }
    }

    /// S-08 follow-up: the app's own page is exactly 127.0.0.1 or localhost
    /// on the live port; nothing on this computer is while no listener is
    /// bound; development adds localhost:5173 only.
    #[test]
    fn the_app_page_is_exactly_the_live_port_on_loopback() {
        use super::{app_page, main_window_may_load};
        let live = 5001;
        for page in ["http://127.0.0.1:5001/dashboard", "http://LOCALHOST:5001/"] {
            assert!(app_page(&u(page), live, false), "{}", page);
            assert!(main_window_may_load(&u(page), live, false), "{}", page);
        }
        for page in [
            "http://127.0.0.1:5000/",
            "http://localhost:8080/",
            "http://127.0.0.2:5001/",
            "http://[::1]:5001/",
            "http://0.0.0.0:5001/",
            "https://127.0.0.1:5001/",
        ] {
            assert!(!app_page(&u(page), live, false), "{}", page);
            assert!(!main_window_may_load(&u(page), live, false), "{}", page);
        }
        // No listener bound: nothing on this computer, the live port's
        // address included.
        for page in ["http://127.0.0.1:5001/", "http://localhost:5001/"] {
            assert!(!app_page(&u(page), 0, false), "{}", page);
            assert!(!main_window_may_load(&u(page), 0, false), "{}", page);
        }
        // Development: the Vite page on localhost:5173 only, even unbound.
        assert!(main_window_may_load(&u("http://localhost:5173/"), 0, true));
        assert!(!main_window_may_load(
            &u("http://127.0.0.1:5173/"),
            live,
            true
        ));
        assert!(!main_window_may_load(
            &u("http://localhost:5173/"),
            live,
            false
        ));
        // Pages not on this computer carry none of the app's permissions and
        // stay reachable (a broker's sign-in page, the bundled page).
        for page in [
            "https://kite.zerodha.com/connect/login",
            "tauri://localhost/index.html",
            "http://tauri.localhost/index.html",
        ] {
            assert!(main_window_may_load(&u(page), 0, false), "{}", page);
            assert!(!app_page(&u(page), live, false), "{}", page);
        }
    }

    /// S-08 follow-up: after the server moves from port A to port B, only
    /// B is the app's page: the window never loads A again and a command
    /// called from a page on A is refused.
    #[test]
    fn after_a_port_change_only_the_new_port_is_trusted() {
        let (a, b) = (5000, 5001);
        assert!(super::main_window_may_load(
            &u("http://127.0.0.1:5000/"),
            a,
            false
        ));
        assert!(!super::main_window_may_load(
            &u("http://127.0.0.1:5000/"),
            b,
            false
        ));
        assert!(super::main_window_may_load(
            &u("http://127.0.0.1:5001/"),
            b,
            false
        ));
        // The grant for the new port names that port only.
        assert!(remote_app_urls(b, false)
            .iter()
            .all(|p| p.contains(":5001/")));
        // A command invoked from a page on the old port is refused.
        assert!(!reset_caller_allowed(
            "main",
            &u("http://127.0.0.1:5000/"),
            b
        ));
        assert!(reset_caller_allowed(
            "main",
            &u("http://127.0.0.1:5001/"),
            b
        ));
        assert!(!reset_caller_allowed(
            "main",
            &u("http://127.0.0.1:5001/"),
            0
        ));
    }

    /// S-08: the permissions granted at run time name permissions that
    /// exist. A wrong name fails `add_capability` at start-up, leaving the
    /// window without them, and no compile-time check covers a capability
    /// built in code. The build script writes the ACL manifests this reads.
    #[test]
    fn runtime_app_permissions_exist_in_the_acl_manifests() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("gen")
            .join("schemas")
            .join("acl-manifests.json");
        let text = std::fs::read_to_string(&path).unwrap();
        let manifests: serde_json::Value = serde_json::from_str(&text).unwrap();
        for p in super::REMOTE_APP_PERMISSIONS {
            let (manifest, name) = p.rsplit_once(':').unwrap_or(("__app-acl__", p));
            assert!(
                manifests[manifest]["permissions"][name].is_object(),
                "{} is not a known permission",
                p
            );
        }
    }

    fn u(s: &str) -> url::Url {
        url::Url::parse(s).unwrap()
    }

    #[test]
    fn only_the_main_window_on_the_app_page_may_reset() {
        assert!(reset_caller_allowed(
            "main",
            &u("http://127.0.0.1:5000/reset-password"),
            5000
        ));
        assert!(reset_caller_allowed(
            "main",
            &u("http://localhost:5000/"),
            5000
        ));
        // Another program on loopback, a broker page, the bundled page, a
        // rebinding host.
        assert!(!reset_caller_allowed(
            "main",
            &u("http://127.0.0.1:8080/"),
            5000
        ));
        assert!(!reset_caller_allowed(
            "main",
            &u("https://kite.zerodha.com/connect"),
            5000
        ));
        assert!(!reset_caller_allowed(
            "main",
            &u("tauri://localhost/index.html"),
            5000
        ));
        assert!(!reset_caller_allowed(
            "main",
            &u("http://evil.example:5000/"),
            5000
        ));
        // A hidden OpenScript runner window on the right page.
        assert!(!reset_caller_allowed(
            "runner-1",
            &u("http://127.0.0.1:5000/"),
            5000
        ));
    }
}

#[cfg(test)]
mod startup_tests {
    /// REL-02: a refused data folder reaches the start-up page as
    /// `data_refused` (startup.js shows it with no port or retry), and an
    /// ordinary status carries no such key.
    #[test]
    fn a_refused_data_folder_reaches_the_start_up_page() {
        let refusal = crate::db::upgrade::Refusal::Downgrade {
            recorded: "1.2.0".into(),
            current: "1.1.0".into(),
        };
        let v = serde_json::to_value(super::data_refused_status(&refusal.message())).unwrap();
        assert_eq!(v["data_refused"], refusal.message());
        assert_eq!(v["state"], "failed");
        assert_eq!(v["ws"]["state"], "starting");
        let js = include_str!("../../bootstrap/startup.js");
        assert!(js.contains("status.data_refused"));
        let ok = super::StartupStatus {
            http: crate::state::ServerStatus::Starting,
            ws: crate::state::ServerStatus::Starting,
            data_refused: None,
        };
        assert!(serde_json::to_value(ok)
            .unwrap()
            .get("data_refused")
            .is_none());
    }
}
