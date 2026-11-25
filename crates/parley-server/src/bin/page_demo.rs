//! `page_demo <transcript-file>`: serve the meeting page and feed one line about every 2 seconds.
//! File lines look like `Speaker: text`; a line with no speaker gets the name "Speaker".
//! Each line typed on stdin is appended to the live doc as a paragraph (the server-side edit).
//! A stdin line that starts with the bot name ("Parley, start a doc for the launch plan") is a request:
//! the model from `PARLEY_LLM` (openai or anthropic, no default) writes the live doc and the chat line is posted.
//! A stdin line `board A,B,C` instead sets the whiteboard to a chain of boxes A to B to C.
//! Set `PAGE_DEMO_ADDR` (default 127.0.0.1:8080) and `PAGE_DEMO_BASE` (default http://<addr>).

use std::time::Duration;

use parley_artifacts::board::{Board, Edge, Node};
use parley_server::{Line, Llm, Meeting, app, new_secret, now_ms};

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
    let doc_meeting = meeting.clone();
    let (router, tx) = app(meeting);
    let chat_tx = tx.clone();
    tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt;
        let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            match l.strip_prefix("board ") {
                Some(chain) => {
                    let names: Vec<&str> = chain.split(',').map(str::trim).collect();
                    let board = Board {
                        nodes: names
                            .iter()
                            .map(|n| Node {
                                id: n.to_string(),
                                label: n.to_string(),
                            })
                            .collect(),
                        edges: names
                            .windows(2)
                            .map(|w| Edge {
                                from: w[0].into(),
                                to: w[1].into(),
                                label: None,
                            })
                            .collect(),
                        groups: vec![],
                    };
                    if let Err(e) = doc_meeting.set_board(&board) {
                        eprintln!("board: {e}");
                    }
                }
                None if parley_brain::find_request(&l, parley_brain::DEFAULT_NAME).is_some() => {
                    let said = match Llm::from_env() {
                        Err(e) => Err(e.to_string()),
                        Ok(llm) => {
                            doc_meeting
                                .request(&llm, parley_brain::DEFAULT_NAME, &l)
                                .await
                        }
                    };
                    match said {
                        Ok(Some(chat)) => {
                            let _ = chat_tx
                                .send(Line {
                                    speaker: parley_brain::DEFAULT_NAME.into(),
                                    text: chat,
                                    at_ms: now_ms(),
                                })
                                .await;
                        }
                        Ok(None) => {}
                        Err(e) => eprintln!("request: {e}"),
                    }
                }
                None => doc_meeting.append_paragraph(&l),
            }
        }
    });
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
