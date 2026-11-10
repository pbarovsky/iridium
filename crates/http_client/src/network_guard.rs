//! A guard around the app's HTTP client, which every request Zed makes goes
//! through. It logs each request, so it's visible what Zed talks to, and
//! refuses all of them while Zed is offline, except ones to this machine.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use parking_lot::Mutex;

use futures::future::BoxFuture;
use http::{HeaderValue, Request, Response};
use url::Url;

use crate::{AsyncBody, HttpClient};

/// The log target of the request log, e.g. for `RUST_LOG=network=info`.
pub const LOG_TARGET: &str = "network";

/// Offline because of `--offline` or `ZED_OFFLINE`, which settings can't undo.
static FORCED_OFFLINE: AtomicBool = AtomicBool::new(false);
/// Offline because of the `network.offline` setting.
static OFFLINE_SETTING: AtomicBool = AtomicBool::new(false);
/// The `network.automatic_updates` setting, for code without access to settings.
static AUTOMATIC_UPDATES: AtomicBool = AtomicBool::new(false);
/// The `network.ask_before_downloading` setting.
static ASK_BEFORE_DOWNLOADING: AtomicBool = AtomicBool::new(true);

/// Asks the user whether Zed may download something, and resolves to their
/// answer. Set by the app, which shows the question as a notification.
pub type DownloadConsentHandler = Arc<dyn Fn(String) -> BoxFuture<'static, bool> + Send + Sync>;

static DOWNLOAD_CONSENT_HANDLER: Mutex<Option<DownloadConsentHandler>> = Mutex::new(None);

pub fn set_download_consent_handler(handler: DownloadConsentHandler) {
    *DOWNLOAD_CONSENT_HANDLER.lock() = Some(handler);
}

pub fn set_ask_before_downloading(ask: bool) {
    ASK_BEFORE_DOWNLOADING.store(ask, Ordering::Relaxed);
}

/// Asks the user whether Zed may download `subject`, e.g. "the vtsls
/// language server", before it installs something for the first time.
/// Resolves to false while offline or without a network, and to true when Zed
/// isn't set to ask.
pub async fn ask_to_download(subject: impl Into<String>) -> bool {
    if is_offline() || !is_network_available() {
        return false;
    }
    if !ASK_BEFORE_DOWNLOADING.load(Ordering::Relaxed) {
        return true;
    }
    let subject = subject.into();
    let handler = DOWNLOAD_CONSENT_HANDLER.lock().clone();
    let Some(handler) = handler else {
        return true;
    };
    log::info!(target: LOG_TARGET, "asking whether to download {subject}");
    let allowed = handler(subject.clone()).await;
    log::info!(
        target: LOG_TARGET,
        "download of {subject} {}",
        if allowed { "allowed" } else { "declined" }
    );
    allowed
}

/// Keeps Zed offline for the rest of the session, whatever the settings say.
pub fn force_offline() {
    FORCED_OFFLINE.store(true, Ordering::Relaxed);
}

/// Whether this machine has a route to the internet, as last checked.
static NETWORK_AVAILABLE: AtomicBool = AtomicBool::new(true);

/// Checks whether this machine has a route to the internet, without sending
/// anything: connecting a UDP socket only looks up a route.
pub fn probe_network() -> bool {
    use std::net::UdpSocket;
    [
        ("0.0.0.0:0", "1.1.1.1:53"),
        ("[::]:0", "[2606:4700:4700::1111]:53"),
    ]
    .iter()
    .any(|(local, remote)| {
        UdpSocket::bind(local).is_ok_and(|socket| socket.connect(remote).is_ok())
    })
}

pub fn set_network_available(available: bool) {
    NETWORK_AVAILABLE.store(available, Ordering::Relaxed);
}

pub fn is_network_available() -> bool {
    NETWORK_AVAILABLE.load(Ordering::Relaxed)
}

/// Whether `--offline` or `ZED_OFFLINE` keeps Zed offline this session.
pub fn is_forced_offline() -> bool {
    FORCED_OFFLINE.load(Ordering::Relaxed)
}

pub fn set_offline_setting(offline: bool) {
    OFFLINE_SETTING.store(offline, Ordering::Relaxed);
}

pub fn set_automatic_updates(automatic_updates: bool) {
    AUTOMATIC_UPDATES.store(automatic_updates, Ordering::Relaxed);
}

/// Whether Zed may check for and download new versions of what is already
/// installed, without being asked.
pub fn allows_automatic_updates() -> bool {
    !is_offline() && AUTOMATIC_UPDATES.load(Ordering::Relaxed)
}

/// Whether Zed must not use the network at all.
pub fn is_offline() -> bool {
    FORCED_OFFLINE.load(Ordering::Relaxed) || OFFLINE_SETTING.load(Ordering::Relaxed)
}

/// The scheme, host and path of a URL, without its query or fragment, which
/// can hold keys or tokens that don't belong in a log.
fn loggable(uri: &http::Uri) -> String {
    let scheme = uri.scheme_str().unwrap_or("https");
    let host = uri.host().unwrap_or_default();
    let port = uri
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    format!("{scheme}://{host}{port}{}", uri.path())
}

/// Whether a request stays on this machine, e.g. for a local model server,
/// which is fine while offline.
fn is_local(uri: &http::Uri) -> bool {
    match uri.host() {
        Some("localhost") => true,
        Some(host) => host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback()),
        None => false,
    }
}

pub struct NetworkGuardClient {
    inner: Arc<dyn HttpClient>,
}

impl NetworkGuardClient {
    pub fn new(inner: Arc<dyn HttpClient>) -> Self {
        Self { inner }
    }
}

impl HttpClient for NetworkGuardClient {
    fn user_agent(&self) -> Option<&HeaderValue> {
        self.inner.user_agent()
    }

    fn proxy(&self) -> Option<&Url> {
        self.inner.proxy()
    }

    fn send(
        &self,
        request: Request<AsyncBody>,
    ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        let target = loggable(request.uri());
        let method = request.method().clone();
        if is_offline() && !is_local(request.uri()) {
            log::info!(target: LOG_TARGET, "blocked, offline: {method} {target}");
            return Box::pin(async move {
                Err(anyhow::anyhow!(
                    "Zed is offline, so it didn't request {target}"
                ))
            });
        }
        log::info!(target: LOG_TARGET, "{method} {target}");
        self.inner.send(request)
    }
}
