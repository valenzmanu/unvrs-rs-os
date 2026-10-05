//! The Observatory (D24, voyage 0.8 C10): an operator view of the kernel,
//! served on loopback by the kernel daemon on its own thread.
//!
//! - `/console` is the live console: Dioxus LiveView over a websocket, plus the Cosmos hero (a
//!   galaxy merger simulated and rasterized in Rust compiled to WebAssembly, see
//!   `cosmos/cosmos.rs`, drawn by a worker on an OffscreenCanvas).
//! - `/static` renders the same console as plain HTML (no JavaScript).
//! - `/api/snapshot` returns the snapshot JSON.
//! - `/api/settings` reads the settings registry and accepts protected set/revert writes.
//! - `/` and `/preview` serve the Observatory (status line, Needs you, Crew, Alive, Fuel over
//!   the dimmed galaxy); `/preview?sim=problem` or `?sim=calm` show labelled fixtures
//!   instead, and `/api/preview` returns the preview's model as JSON (same flags).
//!
//! Settings writes use POST with exact Host/Origin, page CSRF and a server-only credential.
//! Reads retain their existing behavior. Requests must name the server
//! as `unvrs.localhost`, `localhost` or `127.0.0.1` (DNS-rebinding guard), and cross-site
//! pages cannot open the websocket (the Origin, when sent, must be one of those too).
pub mod preview;
pub mod probe;
mod settings;
mod view;
pub use settings::SettingsFn;

use anyhow::{Result, ensure};
use axum::{
    Json, Router,
    extract::{RawQuery, Request, WebSocketUpgrade},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use dioxus::prelude::*;
use serde_json::Value;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::sync::watch;

/// Produces the kernel's current snapshot. Called on a blocking thread.
pub type SnapshotFn = Arc<dyn Fn() -> Value + Send + Sync>;

pub use view::{bytes as fmt_bytes, dur as fmt_duration};

const STYLE: &str = include_str!("style.css");
const COSMOS_JS: &str = include_str!("cosmos.js");
const BOOT_JS: &str = include_str!("boot.js");
const COSMOS_WASM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cosmos.wasm"));
/// The renderer's own still frame, shown in the window when there is no WebGL2 or WASM.
const COSMOS_POSTER: &[u8] = include_bytes!("../cosmos/poster.jpg");

/// How often connected pages see a fresh snapshot.
const TICK: Duration = Duration::from_millis(1000);

/// The ship's window: the cosmos canvas, fixed full-bleed behind the bridge UI. Without
/// JavaScript (or WebGL2) the same layer shows the renderer's still poster instead.
fn window(live: bool) -> String {
    let canvas = if live {
        "<canvas id=\"cosmos\" aria-hidden=\"true\"></canvas>"
    } else {
        ""
    };
    format!(
        "<div class=\"window{}\" id=\"hero\">{canvas}\
<div class=\"hud\"><div class=\"brand\"><span class=\"logo\">UNVRS</span>\
<span class=\"sub\">Bridge{}</span></div>\
<div class=\"readout\" aria-hidden=\"true\"><span class=\"phase\" id=\"cosmos-phase\"></span>\
<span class=\"stats\" id=\"cosmos-stats\"></span></div></div></div>",
        if live { "" } else { " nocosmos" },
        if live { "" } else { " · static" }
    )
}

fn page(body: &str, live: bool) -> String {
    let head_extra = if live {
        String::new()
    } else {
        "<meta http-equiv=\"refresh\" content=\"2\">".to_string()
    };
    let scripts = if live {
        format!(
            "{}<script>{BOOT_JS}</script>",
            dioxus_liveview::interpreter_glue("/ws")
        )
    } else {
        String::new()
    };
    let hero = window(live);
    format!(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<title>UNVRS · Bridge</title>\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<meta name=\"color-scheme\" content=\"dark\"><link rel=\"icon\" href=\"data:,\">{head_extra}\
<style>{STYLE}</style></head><body class=\"{}\">{hero}\
<div id=\"lost\" class=\"lost\" role=\"alert\" hidden>Signal lost · reconnecting</div>\
<div id=\"main\">{body}</div>{scripts}</body></html>",
        if live { "live" } else { "static" }
    )
}

#[derive(Clone)]
struct Hub {
    snapshot: SnapshotFn,
    rx: watch::Receiver<Value>,
}

async fn fetch(f: &SnapshotFn) -> Value {
    let f = f.clone();
    tokio::task::spawn_blocking(move || f())
        .await
        .unwrap_or(Value::Null)
}

fn app(hub: Hub) -> Element {
    let mut snap = use_signal(|| hub.rx.borrow().clone());
    // The deck keeps its place across snapshots: once one has arrived, Page stays mounted
    // even if a later one is empty (it then reads "kernel offline").
    let mut seen = use_signal(|| false);
    use_future(move || {
        let hub = hub.clone();
        async move {
            let mut rx = hub.rx.clone();
            let mut put = move |v: Value| {
                if !v.is_null() && !*seen.peek() {
                    seen.set(true);
                }
                snap.set(v);
            };
            put(fetch(&hub.snapshot).await);
            while rx.changed().await.is_ok() {
                let v = rx.borrow_and_update().clone();
                put(v);
            }
        }
    });
    let v = snap.read().clone();
    if v.is_null() && !seen() {
        return rsx! { div { class: "boot", role: "status", "Connecting to the kernel…" } };
    }
    rsx! { view::Page { snap: v, live: true } }
}

/// Props of one preview page: the shared snapshot hub, the probes, and whether it shows
/// the simulated fixture instead of real data.
#[derive(Clone)]
struct PreviewHub {
    hub: Hub,
    probes: probe::Probes,
    sim: Option<&'static str>,
}

async fn preview_model(p: &PreviewHub, snap: Value) -> preview::model::Model {
    let (probes, sim) = (p.probes.clone(), p.sim);
    tokio::task::spawn_blocking(move || preview::build(&snap, &probes, sim))
        .await
        .unwrap_or_else(|_| preview::build(&Value::Null, &probe::Probes::default(), None))
}

fn preview_app(p: PreviewHub) -> Element {
    let mut model = use_signal(|| None::<preview::model::Model>);
    use_future(move || {
        let p = p.clone();
        async move {
            let mut rx = p.hub.rx.clone();
            // the last good snapshot stays on screen when the kernel stops answering; its
            // age (the heartbeat) then grows until the kernel light goes amber, then red
            let mut last = fetch(&p.hub.snapshot).await;
            loop {
                model.set(Some(preview_model(&p, last.clone()).await));
                if rx.changed().await.is_err() {
                    break;
                }
                let v = rx.borrow_and_update().clone();
                if !v.is_null() {
                    last = v;
                }
            }
        }
    });
    match model() {
        None => rsx! { div { class: "boot", role: "status", "Connecting to the kernel…" } },
        Some(m) => rsx! { preview::Preview { model: m } },
    }
}

/// Is `host` (a Host header, or an Origin without its scheme) one of our names?
fn allowed_host(host: &str) -> bool {
    let name = match host.rsplit_once(':') {
        Some((n, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => n,
        Some(_) => return false,
        None => host,
    };
    matches!(
        name.to_ascii_lowercase().as_str(),
        "unvrs.localhost" | "localhost" | "127.0.0.1"
    )
}

async fn guard(req: Request, next: Next) -> Response {
    let h = req.headers();
    let host_ok = h
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .is_some_and(allowed_host);
    let origin_ok = match h.get(header::ORIGIN) {
        None => true,
        Some(o) => o
            .to_str()
            .ok()
            .and_then(|o| o.strip_prefix("http://"))
            .is_some_and(allowed_host),
    };
    if !host_ok || !origin_ok {
        return (
            StatusCode::FORBIDDEN,
            "unvrs observatory: open it as http://unvrs.localhost:7576\n",
        )
            .into_response();
    }
    let mut res = next.run(req).await;
    let hs = res.headers_mut();
    hs.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    hs.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    hs.entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
    res
}

fn asset(ct: &'static str, body: &'static [u8]) -> Response {
    (
        [
            (header::CONTENT_TYPE, ct),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

/// The routes, without binding: `serve` uses this, and so can tests.
pub fn router(snapshot: SnapshotFn) -> Router {
    let (tx, rx) = watch::channel(Value::Null);
    // One poller for every connected page; it sleeps while nobody watches.
    let poll = snapshot.clone();
    tokio::spawn(async move {
        loop {
            if tx.receiver_count() > 1 {
                let v = fetch(&poll).await;
                if tx.send(v).is_err() {
                    break;
                }
            }
            tokio::time::sleep(TICK).await;
        }
    });
    let hub = Hub {
        snapshot: snapshot.clone(),
        rx,
    };
    let pool = Arc::new(dioxus_liveview::LiveViewPool::new());
    let preview_pool = Arc::new(dioxus_liveview::LiveViewPool::new());
    let probes = probe::Probes::default();
    // start the slow readings (Claude usage, worktree sizes) now, not on the first page
    let (warm_snap, warm_probes) = (snapshot.clone(), probes.clone());
    tokio::spawn(async move {
        let snap = fetch(&warm_snap).await;
        let _ = tokio::task::spawn_blocking(move || preview::warm(&snap, &warm_probes)).await;
    });
    let index = Html(page("", true));
    let s1 = snapshot.clone();
    let s2 = snapshot.clone();
    let s3 = snapshot;
    let (hub2, probes2) = (hub.clone(), probes.clone());
    Router::new()
        .route("/console", get(move || async move { index.clone() }))
        .route(
            "/",
            get(move |RawQuery(q): RawQuery| async move {
                Html(preview::page(
                    preview::wants_sim(q.as_deref()),
                    preview::PREVIEW_CSS,
                    BOOT_JS,
                ))
            }),
        )
        .route(
            "/ws",
            get(move |ws: WebSocketUpgrade| {
                let pool = pool.clone();
                let hub = hub.clone();
                async move {
                    ws.on_upgrade(move |socket| async move {
                        let _ = pool
                            .launch_with_props(dioxus_liveview::axum_socket(socket), app, hub)
                            .await;
                    })
                }
            }),
        )
        .route(
            "/preview",
            get(move |RawQuery(q): RawQuery| async move {
                Html(preview::page(
                    preview::wants_sim(q.as_deref()),
                    preview::PREVIEW_CSS,
                    BOOT_JS,
                ))
            }),
        )
        .route(
            "/preview/ws",
            get(move |RawQuery(q): RawQuery, ws: WebSocketUpgrade| {
                let pool = preview_pool.clone();
                let props = PreviewHub {
                    hub: hub2.clone(),
                    probes: probes2.clone(),
                    sim: preview::wants_sim(q.as_deref()),
                };
                async move {
                    ws.on_upgrade(move |socket| async move {
                        let _ = pool
                            .launch_with_props(
                                dioxus_liveview::axum_socket(socket),
                                preview_app,
                                props,
                            )
                            .await;
                    })
                }
            }),
        )
        .route(
            "/api/preview",
            get(move |RawQuery(q): RawQuery| async move {
                let sim = preview::wants_sim(q.as_deref());
                let snap = if sim.is_some() {
                    Value::Null
                } else {
                    fetch(&s3).await
                };
                let probes = probes.clone();
                let m = tokio::task::spawn_blocking(move || preview::build(&snap, &probes, sim))
                    .await
                    .ok();
                Json(serde_json::to_value(m).unwrap_or(Value::Null))
            }),
        )
        .route(
            "/static",
            get(move || async move {
                let v = fetch(&s1).await;
                let body = dioxus_ssr::render_element(rsx! { view::Page { snap: v, live: false } });
                Html(page(&body, false))
            }),
        )
        .route(
            "/api/snapshot",
            get(move || async move { Json(fetch(&s2).await) }),
        )
        .route(
            "/cosmos.wasm",
            get(|| async { asset("application/wasm", COSMOS_WASM) }),
        )
        .route(
            "/cosmos.js",
            get(|| async { asset("text/javascript; charset=utf-8", COSMOS_JS.as_bytes()) }),
        )
        .route(
            "/cosmos-poster.jpg",
            get(|| async { asset("image/jpeg", COSMOS_POSTER) }),
        )
        .layer(middleware::from_fn(guard))
}

/// Adds authenticated settings callbacks to the existing Observatory routes.
pub fn router_with_settings(snapshot: SnapshotFn, settings: SettingsFn, token: String) -> Router {
    settings::attach(router(snapshot), settings, token)
}

/// Serves the Observatory on `addr` until the process ends. Blocking: builds its own
/// tokio runtime, so the kernel can call it on a thread of its own. `addr` must be a
/// loopback address (the page shows private state).
pub fn serve(addr: SocketAddr, snapshot: Arc<dyn Fn() -> Value + Send + Sync>) -> Result<()> {
    serve_router(addr, move || router(snapshot))
}

pub fn serve_with_settings(
    addr: SocketAddr,
    snapshot: SnapshotFn,
    settings: SettingsFn,
    token: String,
) -> Result<()> {
    serve_router(addr, move || {
        router_with_settings(snapshot, settings, token)
    })
}

fn serve_router(addr: SocketAddr, routes: impl FnOnce() -> Router) -> Result<()> {
    ensure!(
        addr.ip().is_loopback(),
        "the observatory only serves on loopback, not {addr}"
    );
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("observatory")
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, routes()).await?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::allowed_host;

    #[test]
    fn host_guard() {
        for ok in [
            "unvrs.localhost",
            "unvrs.localhost:7576",
            "localhost",
            "LOCALHOST:80",
            "127.0.0.1",
            "127.0.0.1:7576",
        ] {
            assert!(allowed_host(ok), "{ok}");
        }
        for bad in [
            "evil.com",
            "evil.com:7576",
            "unvrs.localhost.evil.com",
            "127.0.0.1.nip.io",
            "localhost:",
            "localhost:x",
            "0.0.0.0:7576",
            "[::1]:7576",
            "",
        ] {
            assert!(!allowed_host(bad), "{bad}");
        }
    }
}
