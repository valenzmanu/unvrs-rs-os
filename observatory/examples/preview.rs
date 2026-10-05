//! The preview kernel: serves the Observatory (with the new `/preview` view) on its own
//! port, fed by a running kernel's real snapshot, read with plain `GET /api/snapshot`.
//! It never writes to the kernel and never replaces it.
//!
//! `cargo run --release -p observatory --example preview [port] [upstream]`
//! (defaults: 7626, 127.0.0.1:7576), then open http://unvrs.localhost:7626/preview
//! (or /preview?sim=problem for the labelled fixture).
use serde_json::Value;
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    sync::Arc,
};

const TIMEOUT: std::time::Duration = observatory::probe::UPSTREAM_TIMEOUT;

/// Decodes a `Transfer-Encoding: chunked` body.
fn unchunk(mut b: &[u8]) -> Vec<u8> {
    let mut out = vec![];
    loop {
        let Some(nl) = b.windows(2).position(|w| w == b"\r\n") else {
            break;
        };
        let size = std::str::from_utf8(&b[..nl])
            .ok()
            .and_then(|s| usize::from_str_radix(s.split(';').next().unwrap_or("").trim(), 16).ok())
            .unwrap_or(0);
        if size == 0 || b.len() < nl + 2 + size {
            break;
        }
        out.extend_from_slice(&b[nl + 2..nl + 2 + size]);
        b = &b[(nl + 2 + size + 2).min(b.len())..];
    }
    out
}

/// One `GET /api/snapshot` from the upstream kernel; Null when it does not answer.
fn get_snapshot(addr: SocketAddr, host: &str) -> Value {
    let run = || -> Option<Value> {
        let mut s = TcpStream::connect_timeout(&addr, TIMEOUT).ok()?;
        s.set_read_timeout(Some(TIMEOUT)).ok()?;
        s.set_write_timeout(Some(TIMEOUT)).ok()?;
        write!(
            s,
            "GET /api/snapshot HTTP/1.1\r\nHost: {host}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
        )
        .ok()?;
        let mut raw = vec![];
        s.read_to_end(&mut raw).ok()?;
        let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
        let head = String::from_utf8_lossy(&raw[..split]).to_ascii_lowercase();
        if !head.starts_with("http/1.1 200") {
            return None;
        }
        let body = &raw[split + 4..];
        let body = if head.contains("transfer-encoding: chunked") {
            unchunk(body)
        } else {
            body.to_vec()
        };
        serde_json::from_slice(&body).ok()
    };
    run().unwrap_or(Value::Null)
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let port: u16 = args.next().and_then(|p| p.parse().ok()).unwrap_or(7626);
    let upstream = args.next().unwrap_or_else(|| "127.0.0.1:7576".into());
    anyhow::ensure!(
        port != 7576,
        "the preview does not take the live Observatory's port"
    );
    let addr = upstream
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow::anyhow!("no address for {upstream}"))?;
    anyhow::ensure!(
        addr.ip().is_loopback(),
        "the upstream kernel must be on loopback"
    );
    let host = format!("unvrs.localhost:{}", addr.port());
    println!(
        "observatory preview: http://unvrs.localhost:{port}/preview  (real data: GET http://{upstream}/api/snapshot, read-only)\n\
         simulated problem state: http://unvrs.localhost:{port}/preview?sim=problem"
    );
    let snapshot: observatory::SnapshotFn = Arc::new(move || get_snapshot(addr, &host));
    // UNVRS_SETTINGS_FIXTURE=<settings.json>: also serve GET/POST /api/settings from that
    // file, in memory (the Settings overlay's dev loop; the kernel serves the real one)
    let Ok(fixture) = std::env::var("UNVRS_SETTINGS_FIXTURE") else {
        return observatory::serve(([127, 0, 0, 1], port).into(), snapshot);
    };
    let settings: Value = serde_json::from_slice(&std::fs::read(&fixture)?)?;
    println!(
        "settings: GET/POST http://unvrs.localhost:{port}/api/settings from {fixture} (in memory)"
    );
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let app = observatory::router(snapshot).merge(fixture_settings::routes(settings));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
        axum::serve(listener, app).await?;
        Ok(())
    })
}

/// A stand-in for the kernel's settings API (docs/design/settings-api.md), kept in memory:
/// `settings.set` and `settings.revert` change the values and add history, refusing writes
/// without the X-UNVRS-CSRF header, read-only keys and unconfirmed sensitive changes.
mod fixture_settings {
    use axum::{
        Json, Router,
        http::{HeaderMap, StatusCode},
        routing::get,
    };
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};

    fn refuse(code: StatusCode, msg: &str) -> (StatusCode, Json<Value>) {
        (code, Json(json!({"ok": false, "error": msg})))
    }

    fn apply(s: &mut Value, req: &Value) -> Result<Value, (StatusCode, &'static str)> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let confirm = req["confirm"].as_bool().unwrap_or(false);
        let (key, value, reverts) = match req["op"].as_str() {
            Some("settings.set") => (
                req["key"].as_str().unwrap_or("").to_owned(),
                req["value"].clone(),
                Value::Null,
            ),
            Some("settings.revert") => {
                let id = req["change_id"].as_str().unwrap_or("");
                let h = s["history"].as_array().into_iter().flatten();
                let Some(c) = h.into_iter().find(|c| c["change_id"] == id).cloned() else {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "There is no such change to revert.",
                    ));
                };
                let key = c["key"].as_str().unwrap_or("").to_owned();
                if s["values"][&key] != c["after"] {
                    return Err((
                        StatusCode::CONFLICT,
                        "This setting has changed since; revert the newer change first.",
                    ));
                }
                (key, c["before"].clone(), c["change_id"].clone())
            }
            _ => return Err((StatusCode::BAD_REQUEST, "Unknown operation.")),
        };
        let schema = s["schema"].as_array().cloned().unwrap_or_default();
        let Some(e) = schema.iter().find(|e| e["key"] == key.as_str()) else {
            return Err((
                StatusCode::BAD_REQUEST,
                "There is no setting with that key.",
            ));
        };
        if e["editable"] != true {
            return Err((
                StatusCode::BAD_REQUEST,
                "This setting is read-only in this version.",
            ));
        }
        if e["sensitive"] == true && !confirm {
            return Err((
                StatusCode::BAD_REQUEST,
                "This change can weaken safeguards; confirm it to save.",
            ));
        }
        if let Some(a) = e["allowed"].as_array()
            && e["type"] == "enum"
            && !a.contains(&value)
        {
            return Err((
                StatusCode::BAD_REQUEST,
                "That value is not one of the allowed choices.",
            ));
        }
        if let (Some(n), Some(min)) = (value.as_i64(), e["range"]["min"].as_i64())
            && n < min
        {
            return Err((
                StatusCode::BAD_REQUEST,
                "That number is below the smallest allowed value.",
            ));
        }
        let n = s["history"].as_array().map(|h| h.len()).unwrap_or(0) + 1;
        let change = json!({
            "change_id": format!("settings-{now}-{n}"), "key": key,
            "before": s["values"][&key].clone(), "after": value, "by": "observatory",
            "at": now, "reverts": reverts,
        });
        s["values"][&key] = value;
        s["effective_source"][&key] = json!("file");
        if let Some(h) = s["history"].as_array_mut() {
            h.insert(0, change.clone());
        }
        Ok(change)
    }

    pub fn routes(settings: Value) -> Router {
        let state = Arc::new(Mutex::new(settings));
        let (r, w) = (state.clone(), state);
        Router::new().route(
            "/api/settings",
            get(move || async move { Json(r.lock().unwrap().clone()) }).post(
                move |h: HeaderMap, body: String| async move {
                    let csrf = h
                        .get("x-unvrs-csrf")
                        .and_then(|v| v.to_str().ok())
                        .is_some_and(|v| !v.is_empty());
                    if !csrf {
                        return refuse(
                            StatusCode::FORBIDDEN,
                            "This page has no permission to change settings.",
                        );
                    }
                    let Ok(req) = serde_json::from_str::<Value>(&body) else {
                        return refuse(StatusCode::BAD_REQUEST, "The request was not valid JSON.");
                    };
                    match apply(&mut w.lock().unwrap(), &req) {
                        Ok(change) => (StatusCode::OK, Json(json!({"ok": true, "change": change}))),
                        Err((code, msg)) => refuse(code, msg),
                    }
                },
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn chunked_bodies_decode() {
        assert_eq!(
            super::unchunk(b"4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n"),
            b"{\"a\":1}"
        );
    }
}
