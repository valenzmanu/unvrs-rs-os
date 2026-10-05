//! Settings HTTP boundary: exact origin, page CSRF, and server-only kernel credential.
use anyhow::Result;
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Request, State, rejection::JsonRejection},
    http::{HeaderMap, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::Read,
    sync::{Arc, Mutex},
};

pub type SettingsFn = Arc<dyn Fn(Value, &str) -> Result<Value> + Send + Sync>;
const HOST: &str = "unvrs.localhost:7576";
const ORIGIN: &str = "http://unvrs.localhost:7576";

#[derive(Clone)]
struct Settings {
    call: SettingsFn,
    token: Arc<String>,
    pages: Arc<Mutex<VecDeque<String>>>,
}
fn error(status: StatusCode, message: impl ToString) -> Response {
    (
        status,
        Json(json!({"ok":false,"error":message.to_string()})),
    )
        .into_response()
}
fn header_is(headers: &HeaderMap, key: &str, value: &str) -> bool {
    headers.get_all(key).iter().count() == 1
        && headers.get(key).and_then(|h| h.to_str().ok()) == Some(value)
}
async fn boundary(State(settings): State<Settings>, req: Request, next: Next) -> Response {
    let exact_host = header_is(req.headers(), "host", HOST);
    let is_page = req.method() == Method::GET && exact_host;
    if req.method() == Method::POST && req.uri().path() == "/api/settings" {
        if !exact_host || !header_is(req.headers(), "origin", ORIGIN) {
            return error(
                StatusCode::FORBIDDEN,
                "Open settings at http://unvrs.localhost:7576; writes require that exact Host and Origin",
            );
        }
        let csrf = req
            .headers()
            .get("x-unvrs-csrf")
            .and_then(|v| v.to_str().ok());
        if req.headers().get_all("x-unvrs-csrf").iter().count() != 1
            || !csrf.is_some_and(|v| {
                settings
                    .pages
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|known| known == v)
            })
        {
            return error(
                StatusCode::FORBIDDEN,
                "Settings page authorization expired or is missing; reload the Observatory",
            );
        }
    }
    let res = next.run(req).await;
    if !is_page
        || !res.status().is_success()
        || !res
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/html"))
    {
        return res;
    }
    let (mut parts, body) = res.into_parts();
    let body = match to_bytes(body, 8 * 1024 * 1024).await {
        Ok(body) => body,
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Cannot read Observatory page",
            );
        }
    };
    let html = match String::from_utf8(body.to_vec()) {
        Ok(html) => html,
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Cannot read Observatory page text",
            );
        }
    };
    let mut bytes = [0u8; 32];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .is_err()
    {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Cannot authorize the settings page",
        );
    }
    let csrf: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    {
        let mut pages = settings.pages.lock().unwrap();
        // ponytail: retain 1024 open pages per server; oldest pages reload if this limit is reached.
        if pages.len() == 1024 {
            pages.pop_front();
        }
        pages.push_back(csrf.clone());
    }
    let html = html.replacen(
        "</head>",
        &format!("<meta name=\"unvrs-settings-csrf\" content=\"{csrf}\"></head>"),
        1,
    );
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(html))
}
async fn call(settings: Settings, request: Value, write: bool) -> Response {
    let result = tokio::task::spawn_blocking(move || {
        (settings.call)(request, if write { &settings.token } else { "" })
    })
    .await;
    match result {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(e)) => {
            let message = format!("{e:#}");
            let status = if message.starts_with("Refused:") {
                StatusCode::FORBIDDEN
            } else if message.starts_with("Conflict:") {
                StatusCode::CONFLICT
            } else if e.chain().any(|cause| cause.is::<std::io::Error>())
                || message.starts_with("Kernel journal is unavailable")
                || message.starts_with("Settings saved as")
                || !write
            {
                StatusCode::INTERNAL_SERVER_ERROR
            } else {
                StatusCode::BAD_REQUEST
            };
            error(status, message)
        }
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Settings operation failed",
        ),
    }
}
async fn read(State(settings): State<Settings>) -> Response {
    call(settings, json!({"op":"settings.get"}), false).await
}
async fn write(
    State(settings): State<Settings>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let request = match body {
        Ok(Json(v)) => v,
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "Send a JSON settings request with Content-Type: application/json",
            );
        }
    };
    if !matches!(
        request["op"].as_str(),
        Some("settings.set" | "settings.revert")
    ) {
        return error(
            StatusCode::BAD_REQUEST,
            "POST settings accepts settings.set or settings.revert",
        );
    }
    call(settings, request, true).await
}
pub(super) fn attach(router: Router, callback: SettingsFn, token: String) -> Router {
    let settings = Settings {
        call: callback,
        token: Arc::new(token),
        pages: Default::default(),
    };
    router
        .merge(
            Router::new()
                .route("/api/settings", get(read).post(write))
                .with_state(settings.clone())
                .layer(middleware::from_fn(super::guard)),
        )
        .layer(middleware::from_fn_with_state(settings, boundary))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::Html;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;
    const TOKEN: &str = "private-server-credential-never-returned";
    async fn request(
        router: &Router,
        method: &str,
        uri: &str,
        host: &str,
        origin: Option<&str>,
        csrf: Option<&str>,
        body: &str,
    ) -> (StatusCode, String) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", host);
        if let Some(origin) = origin {
            builder = builder.header("origin", origin);
        }
        if let Some(csrf) = csrf {
            builder = builder.header("x-unvrs-csrf", csrf);
        }
        if method == "POST" {
            builder = builder.header("content-type", "application/json");
        }
        let response = router
            .clone()
            .oneshot(builder.body(Body::from(body.to_owned())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let text = String::from_utf8(
            to_bytes(response.into_body(), 8 * 1024 * 1024)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(!text.contains(TOKEN), "server credential leaked");
        assert!(!text.contains("unvrs-settings-token"), "token meta leaked");
        (status, text)
    }
    fn csrf(html: &str) -> String {
        html.split("name=\"unvrs-settings-csrf\" content=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .to_owned()
    }
    fn pages() -> Router {
        Router::new()
            .route(
                "/",
                get(|| async {
                    Html(super::super::preview::page(
                        None,
                        super::super::preview::PREVIEW_CSS,
                        super::super::BOOT_JS,
                    ))
                }),
            )
            .route(
                "/api/snapshot",
                get(|| async { Json(json!({"existing":"snapshot"})) }),
            )
            .layer(middleware::from_fn(super::super::guard))
    }
    #[tokio::test]
    async fn settings_http_refuses_csrf_and_cross_site_without_calling_kernel() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let router = attach(
            pages(),
            Arc::new(move |req, credential| {
                seen.fetch_add(1, Ordering::SeqCst);
                assert_eq!(req["op"], "settings.get");
                assert_eq!(credential, "");
                Ok(
                    json!({"schema":[],"values":{},"effective_source":{},"live_catalog":{},"history":[]}),
                )
            }),
            TOKEN.into(),
        );
        let (status, html) = request(&router, "GET", "/", HOST, None, None, "").await;
        assert_eq!(status, StatusCode::OK);
        let page = csrf(&html);
        assert_eq!(page.len(), 64);
        let (_, second) = request(&router, "GET", "/", HOST, None, None, "").await;
        assert_ne!(page, csrf(&second));
        for alias in ["localhost:7576", "127.0.0.1:7576", "unvrs.localhost:7626"] {
            let (status, html) = request(&router, "GET", "/", alias, None, None, "").await;
            assert_eq!(status, StatusCode::OK);
            assert!(!html.contains("<meta name=\"unvrs-settings-csrf\""));
        }
        let body = r#"{"op":"settings.set","key":"econ.reasoning.deep","value":"medium"}"#;
        for (host, origin, token) in [
            (HOST, Some(ORIGIN), None),
            (HOST, Some(ORIGIN), Some("wrong")),
            (HOST, Some(ORIGIN), Some(TOKEN)),
            (HOST, None, Some(page.as_str())),
            (HOST, Some("null"), Some(page.as_str())),
            (
                HOST,
                Some("https://unvrs.localhost:7576"),
                Some(page.as_str()),
            ),
            (HOST, Some("http://evil.com"), Some(page.as_str())),
            (HOST, Some("http://localhost:7576"), Some(page.as_str())),
            (
                HOST,
                Some("http://unvrs.localhost:7626"),
                Some(page.as_str()),
            ),
            ("localhost:7576", Some(ORIGIN), Some(page.as_str())),
            ("unvrs.localhost:7626", Some(ORIGIN), Some(page.as_str())),
            ("evil.com", Some(ORIGIN), Some(page.as_str())),
        ] {
            let (status, response) =
                request(&router, "POST", "/api/settings", host, origin, token, body).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{host} {origin:?}");
            assert_eq!(
                serde_json::from_str::<Value>(&response).unwrap()["ok"],
                false
            );
        }
        for (name, value) in [
            ("host", HOST),
            ("origin", ORIGIN),
            ("x-unvrs-csrf", page.as_str()),
        ] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/settings")
                        .header("host", HOST)
                        .header("origin", ORIGIN)
                        .header("x-unvrs-csrf", &page)
                        .header(name, value)
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "duplicate {name}");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let (status, response) = request(
            &router,
            "GET",
            "/api/settings",
            "localhost:7576",
            None,
            None,
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(!response.contains(&page));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let (status, response) = request(
            &router,
            "GET",
            "/api/snapshot",
            "127.0.0.1:7576",
            None,
            None,
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            serde_json::from_str::<Value>(&response).unwrap()["existing"],
            "snapshot"
        );
        let restarted = attach(
            pages(),
            Arc::new(|_, _| panic!("expired CSRF reached kernel")),
            TOKEN.into(),
        );
        assert_eq!(
            request(
                &restarted,
                "POST",
                "/api/settings",
                HOST,
                Some(ORIGIN),
                Some(&page),
                body
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    #[tokio::test]
    async fn settings_http_set_read_validate_history_and_revert() {
        let home = std::env::temp_dir().join(format!(
            "settings-http-{}-{}",
            std::process::id(),
            uke::now_ms()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("econ.toml"),
            uke::drv_econ::DEFAULT_CONFIG.replace(
                "refresh_minutes = 5",
                "# captain comment\nrefresh_minutes = 5 # retained",
            ),
        )
        .unwrap();
        let callback_home = home.clone();
        let router = attach(
            pages(),
            Arc::new(move |req, credential| {
                if req["op"] == "settings.get" {
                    assert_eq!(credential, "");
                    return Ok(serde_json::to_value(uke::settings::get(
                        &callback_home,
                        json!({"codex":{"signed_in":true}}),
                    )?)?);
                }
                assert_eq!(credential, TOKEN);
                // Use the real registry/storage here; kernel callback authorization/journaling has its own test.
                let confirm = req["confirm"].as_bool().unwrap_or(false);
                let change = match req["op"].as_str().unwrap() {
                    "settings.set" => uke::settings::set(
                        &callback_home,
                        req["key"].as_str().unwrap(),
                        req["value"].clone(),
                        confirm,
                        "observatory",
                    )?,
                    "settings.revert" => uke::settings::revert(
                        &callback_home,
                        req["change_id"].as_str().unwrap(),
                        confirm,
                        "observatory",
                    )?,
                    _ => anyhow::bail!("Unknown operation"),
                };
                Ok(json!({"ok":true,"change":change}))
            }),
            TOKEN.into(),
        );
        let page = csrf(&request(&router, "GET", "/", HOST, None, None, "").await.1);
        for (body, expected) in [
            (
                r#"{"op":"settings.set","key":"econ.catalog.refresh_minutes","value":0}"#,
                StatusCode::BAD_REQUEST,
            ),
            (
                r#"{"op":"settings.set","key":"safety.watchdog_grace_ms","value":1}"#,
                StatusCode::BAD_REQUEST,
            ),
            (
                r#"{"op":"settings.set","key":"econ.policy.allow_max","value":true}"#,
                StatusCode::BAD_REQUEST,
            ),
            (r#"{"op":"settings.get"}"#, StatusCode::BAD_REQUEST),
            ("not json", StatusCode::BAD_REQUEST),
        ] {
            let (status, response) = request(
                &router,
                "POST",
                "/api/settings",
                HOST,
                Some(ORIGIN),
                Some(&page),
                body,
            )
            .await;
            assert_eq!(status, expected, "{response}");
        }
        assert!(uke::settings::history(&home).unwrap().is_empty());
        let (status, response) = request(
            &router,
            "POST",
            "/api/settings",
            HOST,
            Some(ORIGIN),
            Some(&page),
            r#"{"op":"settings.set","key":"econ.catalog.refresh_minutes","value":7}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{response}");
        let receipt: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(receipt["change"]["by"], "observatory");
        let (_, response) = request(&router, "GET", "/api/settings", HOST, None, None, "").await;
        let saved: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(saved["values"]["econ.catalog.refresh_minutes"], 7);
        assert_eq!(
            saved["history"][0]["change_id"],
            receipt["change"]["change_id"]
        );
        assert_eq!(saved["live_catalog"]["codex"]["signed_in"], true);
        assert!(
            std::fs::read_to_string(home.join("econ.toml"))
                .unwrap()
                .contains("refresh_minutes = 7 # retained")
        );
        let body =
            json!({"op":"settings.revert","change_id":receipt["change"]["change_id"]}).to_string();
        let (status, response) = request(
            &router,
            "POST",
            "/api/settings",
            HOST,
            Some(ORIGIN),
            Some(&page),
            &body,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{response}");
        assert_eq!(
            serde_json::from_str::<Value>(&response).unwrap()["change"]["after"],
            5
        );
        let (status, response) = request(
            &router,
            "POST",
            "/api/settings",
            HOST,
            Some(ORIGIN),
            Some(&page),
            &body,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{response}");
        assert_eq!(uke::settings::history(&home).unwrap().len(), 2);
        std::fs::remove_dir_all(home).unwrap();
    }
}
