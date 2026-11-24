//! `page_demo <transcript-file>`: serve the meeting page and feed one line about every 2 seconds.
//! File lines look like `Speaker: text`; a line with no speaker gets the name "Speaker".
//! Set `PAGE_DEMO_ADDR` (default 127.0.0.1:8080) and `PAGE_DEMO_BASE` (default http://<addr>).

use std::time::Duration;

use parley_server::{Line, Meeting, app, new_secret, now_ms};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: page_demo <transcript-file>")?;
    let text = std::fs::read_to_string(&path)?;
    let addr = std::env::var("PAGE_DEMO_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let base = std::env::var("PAGE_DEMO_BASE").unwrap_or_else(|_| format!("http://{addr}"));

    let meeting = Meeting::new(new_secret());
    let url = format!("{base}/m/{}", meeting.secret());
    let (router, tx) = app(meeting);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("{url}");

    tokio::spawn(async move {
        for raw in text.lines().filter(|l| !l.trim().is_empty()) {
            let (speaker, body) = match raw.split_once(':') {
                Some((s, b)) if s.len() <= 40 => (s.trim(), b.trim()),
                _ => ("Speaker", raw.trim()),
            };
            let line = Line {
                speaker: speaker.into(),
                text: body.into(),
                at_ms: now_ms(),
            };
            if tx.send(line).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
    axum::serve(listener, router).await?;
    Ok(())
}
