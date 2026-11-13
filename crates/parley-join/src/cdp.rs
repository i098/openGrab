//! A thin Chrome DevTools Protocol client over one WebSocket.
//!
//! It sends only the commands the caller asks for. In particular it never sends `Runtime.enable`:
//! Meet refuses a guest join when that domain is on (see README), and the driver needs none of its
//! events (`Runtime.evaluate` and `Page.addScriptToEvaluateOnNewDocument` work without it).

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>;

/// A busy host stalls CDP replies for tens of seconds.
const CALL_TIMEOUT: Duration = Duration::from_secs(90);

pub struct Cdp {
    out: mpsc::UnboundedSender<String>,
    pending: Pending,
    next: std::sync::atomic::AtomicU64,
    tasks: [JoinHandle<()>; 2],
}

impl Cdp {
    pub async fn connect(url: &str) -> Result<Self> {
        let (ws, _) = tokio_tungstenite::connect_async(url)
            .await
            .with_context(|| format!("connecting to the DevTools socket {url}"))?;
        let (mut sink, mut stream) = ws.split();
        let (out, mut rx) = mpsc::unbounded_channel::<String>();
        let writer = tokio::spawn(async move {
            while let Some(m) = rx.recv().await {
                if sink.send(Message::text(m)).await.is_err() {
                    return;
                }
            }
        });
        let pending = Pending::default();
        let table = pending.clone();
        let reader = tokio::spawn(async move {
            while let Some(Ok(Message::Text(t))) = stream.next().await {
                if let Ok(v) = serde_json::from_str::<Value>(&t) {
                    route(&table, v);
                }
            }
            // The socket is gone: wake every caller instead of letting it wait for the timeout.
            table.lock().unwrap().clear();
        });
        Ok(Self {
            out,
            pending,
            next: 1.into(),
            tasks: [writer, reader],
        })
    }

    /// Send one command (on `session` when given) and wait for its result.
    pub async fn call(&self, session: Option<&str>, method: &str, params: Value) -> Result<Value> {
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let mut msg = json!({"id": id, "method": method, "params": params});
        if let Some(s) = session {
            msg["sessionId"] = s.into();
        }
        if self.out.send(msg.to_string()).is_err() {
            self.pending.lock().unwrap().remove(&id);
            bail!("{method}: the browser connection is closed");
        }
        match tokio::time::timeout(CALL_TIMEOUT, rx).await {
            Ok(Ok(r)) => r.with_context(|| method.to_string()),
            Ok(Err(_)) => bail!("{method}: the browser connection closed"),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(anyhow!("{method}: no reply in {}s", CALL_TIMEOUT.as_secs()))
            }
        }
    }
}

impl Drop for Cdp {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

/// The browser WebSocket URL of a Chromium started with `--remote-debugging-port=<port>`.
/// Polls `/json/version` until Chromium answers or `within` has passed.
pub async fn browser_socket(port: u16, within: Duration) -> Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let deadline = tokio::time::Instant::now() + within;
    loop {
        // Chromium may keep the connection open, so stop reading as soon as the JSON body parses.
        let reply = tokio::time::timeout(Duration::from_secs(3), async {
            let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
            // Chromium builds the socket URL from the Host header, so it must carry the port.
            let req = format!(
                "GET /json/version HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
            );
            s.write_all(req.as_bytes()).await?;
            let mut buf = Vec::new();
            loop {
                if s.read_buf(&mut buf).await? == 0 {
                    bail!("closed before the reply was complete");
                }
                let text = String::from_utf8_lossy(&buf);
                if let Some((_, body)) = text.split_once("\r\n\r\n")
                    && let Ok(v) = serde_json::from_str::<Value>(body)
                    && let Some(url) = v["webSocketDebuggerUrl"].as_str()
                {
                    return anyhow::Ok(url.to_string());
                }
            }
        })
        .await;
        if let Ok(Ok(url)) = reply {
            return Ok(url);
        }
        if tokio::time::Instant::now() > deadline {
            bail!(
                "Chromium did not open its DevTools port {port} in {}s",
                within.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Hand a reply to the caller waiting for its id. Events (no id) are ignored.
fn route(table: &Pending, v: Value) {
    let Some(id) = v["id"].as_u64() else { return };
    let Some(tx) = table.lock().unwrap().remove(&id) else {
        return;
    };
    let r = match v.get("error") {
        Some(e) => Err(anyhow!("{}", e["message"].as_str().unwrap_or("CDP error"))),
        None => Ok(v["result"].clone()),
    };
    let _ = tx.send(r);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn routes_results_and_errors_by_id_and_sends_the_session() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
            while let Some(Ok(Message::Text(t))) = ws.next().await {
                let v: Value = serde_json::from_str(&t).unwrap();
                let reply = if v["method"] == "Bad.call" {
                    json!({"id": v["id"], "error": {"message": "nope"}})
                } else {
                    json!({"id": v["id"], "result": {"session": v["sessionId"]}})
                };
                ws.send(Message::text(reply.to_string())).await.unwrap();
            }
        });
        let cdp = Cdp::connect(&url).await.unwrap();
        let ok = cdp.call(Some("S1"), "A.b", json!({})).await.unwrap();
        assert_eq!(ok["session"], "S1");
        let err = cdp.call(None, "Bad.call", json!({})).await.unwrap_err();
        assert!(format!("{err:#}").contains("nope"));
    }
}
