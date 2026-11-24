use crate::{Delta, DeltaStream, LlmError};
use futures_util::{Stream, StreamExt, stream};
use std::collections::VecDeque;

pub(crate) struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Turns SSE events into deltas. `done` turns true after the `Delta::End` was produced.
pub(crate) trait Decoder: Send + 'static {
    fn event(&mut self, ev: SseEvent) -> Result<Vec<Delta>, LlmError>;
    fn done(&self) -> bool;
}

/// Incremental SSE framing: feed bytes, take complete events.
#[derive(Default)]
struct Framer {
    buf: Vec<u8>,
}

impl Framer {
    fn push(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some((end, sep)) = find_blank_line(&self.buf) {
            let block = String::from_utf8_lossy(&self.buf[..end]).into_owned();
            self.buf.drain(..end + sep);
            let (mut event, mut data) = (None, Vec::new());
            for line in block.lines() {
                if let Some(v) = line.strip_prefix("data:") {
                    data.push(v.strip_prefix(' ').unwrap_or(v).to_owned());
                } else if let Some(v) = line.strip_prefix("event:") {
                    event = Some(v.trim().to_owned());
                }
            }
            if !data.is_empty() {
                out.push(SseEvent {
                    event,
                    data: data.join("\n"),
                });
            }
        }
        out
    }
}

fn find_blank_line(b: &[u8]) -> Option<(usize, usize)> {
    (0..b.len()).find_map(|i| {
        if b[i..].starts_with(b"\n\n") {
            Some((i, 2))
        } else if b[i..].starts_with(b"\r\n\r\n") {
            Some((i, 4))
        } else {
            None
        }
    })
}

struct State<B, D> {
    body: B,
    framer: Framer,
    decoder: D,
    queue: VecDeque<Result<Delta, LlmError>>,
    finished: bool,
}

/// Decodes an SSE body. A body that ends before the decoder is done yields `StreamCut`.
pub(crate) fn delta_stream<B, E, D>(body: B, decoder: D) -> DeltaStream
where
    B: Stream<Item = Result<bytes::Bytes, E>> + Send + Unpin + 'static,
    LlmError: From<E>,
    D: Decoder,
{
    let state = State {
        body,
        framer: Framer::default(),
        decoder,
        queue: VecDeque::new(),
        finished: false,
    };
    Box::pin(stream::unfold(state, |mut s| async move {
        loop {
            if let Some(item) = s.queue.pop_front() {
                return Some((item, s));
            }
            if s.finished {
                return None;
            }
            match s.body.next().await {
                Some(Ok(chunk)) => {
                    for ev in s.framer.push(chunk.as_ref()) {
                        match s.decoder.event(ev) {
                            Ok(ds) => s.queue.extend(ds.into_iter().map(Ok)),
                            Err(e) => {
                                s.queue.push_back(Err(e));
                                s.finished = true;
                                break;
                            }
                        }
                        if s.decoder.done() {
                            s.finished = true;
                            break;
                        }
                    }
                }
                Some(Err(e)) => {
                    s.queue.push_back(Err(e.into()));
                    s.finished = true;
                }
                None => {
                    s.queue.push_back(Err(LlmError::StreamCut));
                    s.finished = true;
                }
            }
        }
    }))
}
