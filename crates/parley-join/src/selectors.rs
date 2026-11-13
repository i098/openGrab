//! The one file of Meet page selectors (R2): `page/selectors.json`, compiled in.

use anyhow::{Context, Result, ensure};
use std::collections::BTreeMap;

pub const SELECTORS_JSON: &str = include_str!("../page/selectors.json");

/// Keys the page script reads. Dropping one from the file must fail here, not in a live call.
const REQUIRED: &[&str] = &[
    "nameInput",
    "joinButtonText",
    "leaveButton",
    "micButton",
    "camButton",
    "participantTile",
    "selfMarker",
    "tileName",
    "speakingClass",
    "chatToggle",
    "chatContainer",
    "chatMessage",
    "chatSender",
    "chatText",
    "chatComposer",
    "chatSendIcon",
    "waitingText",
    "deniedText",
    "expiredText",
    "refusedText",
    "notFoundText",
    "endedText",
];

/// Parse the selector file and check every required key holds at least one non-empty entry.
pub fn load() -> Result<BTreeMap<String, Vec<String>>> {
    let mut all: BTreeMap<String, serde_json::Value> =
        serde_json::from_str(SELECTORS_JSON).context("selectors.json is not a JSON object")?;
    all.remove("_comment");
    let mut out = BTreeMap::new();
    for (k, v) in all {
        let list: Vec<String> = serde_json::from_value(v)
            .with_context(|| format!("selectors.json key {k} is not a list of strings"))?;
        out.insert(k, list);
    }
    for key in REQUIRED {
        let list = out
            .get(*key)
            .with_context(|| format!("selectors.json lacks {key}"))?;
        ensure!(
            !list.is_empty() && list.iter().all(|s| !s.trim().is_empty()),
            "selectors.json key {key} is empty"
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn shipped_file_loads_with_every_required_key() {
        let s = super::load().unwrap();
        assert!(s["participantTile"][0].contains("data-participant-id"));
    }
}
