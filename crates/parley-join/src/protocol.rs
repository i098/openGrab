//! Messages from the injected page script to the driver over the local WebSocket.
//!
//! Binary frame (audio), written by `page/inject.js` `frame()`:
//! `u8 tag=1 | u16 LE track | u64 LE end-of-frame time in ms since the Unix epoch | i16 LE samples @ 16 kHz mono`.
//! Text frame: one JSON object tagged by `t`, see [`PageMsg`].

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const AUDIO_TAG: u8 = 1;
pub const HEADER_LEN: usize = 11;
pub const SAMPLE_RATE: u32 = 16_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioFrame {
    pub track: u16,
    pub end_ms: u64,
    pub pcm: Vec<i16>,
}

impl AudioFrame {
    pub fn decode(b: &[u8]) -> Result<Self> {
        ensure!(
            b.len() >= HEADER_LEN,
            "audio frame shorter than its {HEADER_LEN}-byte header"
        );
        if b[0] != AUDIO_TAG {
            bail!("unknown binary frame tag {}", b[0]);
        }
        let body = &b[HEADER_LEN..];
        ensure!(
            body.len().is_multiple_of(2),
            "audio frame has an odd number of sample bytes"
        );
        Ok(Self {
            track: u16::from_le_bytes([b[1], b[2]]),
            end_ms: u64::from_le_bytes(b[3..11].try_into()?),
            pcm: body
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| i16::from_le_bytes(*c))
                .collect(),
        })
    }
}

/// One tile as the Meet page shows it. Every field the page gives about a person is kept
/// for the Outsider check, so `attrs` and `html` hold the raw tile data.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Participant {
    pub id: String,
    pub name: Option<String>,
    pub is_self: bool,
    pub speaking: bool,
    pub avatar: Option<String>,
    #[serde(default)]
    pub attrs: BTreeMap<String, String>,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub html: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum PageMsg {
    Chat {
        id: String,
        sender: String,
        text: String,
    },
    Roster {
        participants: Vec<Participant>,
    },
    Speaking {
        names: Vec<String>,
        at: u64,
    },
    Track {
        id: u16,
        label: String,
        stream: String,
    },
    Log {
        msg: String,
    },
}

#[derive(Debug, Clone)]
pub enum CallEvent {
    Audio(AudioFrame),
    Page(PageMsg),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_bytes(track: u16, ms: u64, samples: &[i16]) -> Vec<u8> {
        let mut b = vec![AUDIO_TAG];
        b.extend(track.to_le_bytes());
        b.extend(ms.to_le_bytes());
        b.extend(samples.iter().flat_map(|s| s.to_le_bytes()));
        b
    }

    #[test]
    fn decodes_the_layout_the_page_script_writes() {
        let f = AudioFrame::decode(&frame_bytes(
            3,
            1_794_556_000_123,
            &[0, 1, -1, i16::MIN, i16::MAX],
        ))
        .unwrap();
        assert_eq!((f.track, f.end_ms), (3, 1_794_556_000_123));
        assert_eq!(f.pcm, [0, 1, -1, i16::MIN, i16::MAX]);
    }

    #[test]
    fn rejects_malformed_frames() {
        assert!(AudioFrame::decode(&[AUDIO_TAG; 5]).is_err(), "short header");
        let mut odd = frame_bytes(1, 0, &[7]);
        odd.pop();
        assert!(AudioFrame::decode(&odd).is_err(), "odd sample bytes");
        let mut bad = frame_bytes(1, 0, &[7]);
        bad[0] = 9;
        assert!(AudioFrame::decode(&bad).is_err(), "unknown tag");
    }

    #[test]
    fn parses_page_messages() {
        let chat: PageMsg =
            serde_json::from_str(r#"{"t":"chat","id":"m1","sender":"Host","text":"hi"}"#).unwrap();
        assert!(matches!(chat, PageMsg::Chat { ref sender, .. } if sender == "Host"));
        let roster: PageMsg = serde_json::from_str(
            r#"{"t":"roster","participants":[{"id":"spaces/a/devices/1","name":null,"is_self":false,"speaking":true,"avatar":null}]}"#,
        )
        .unwrap();
        assert!(matches!(roster, PageMsg::Roster { ref participants } if participants[0].speaking));
    }
}
