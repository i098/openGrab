//! Scoring: word error rate and per-word speaker accuracy. Pure functions, no model files.

/// Upper-case, keep letters, digits and apostrophes, everything else splits words.
pub fn normalize(text: &str) -> Vec<String> {
    text.replace('\u{2019}', "'")
        .to_uppercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Word-level Levenshtein distance (substitution, insertion, deletion all cost 1).
pub fn edit_distance(reference: &[String], hypothesis: &[String]) -> usize {
    let mut prev: Vec<usize> = (0..=hypothesis.len()).collect();
    for (i, r) in reference.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, h) in hypothesis.iter().enumerate() {
            let sub = prev[j] + usize::from(r != h);
            cur.push(sub.min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[hypothesis.len()]
}

/// `(errors, reference words)` after normalizing both sides.
pub fn wer(reference: &str, hypothesis: &str) -> (usize, usize) {
    let (r, h) = (normalize(reference), normalize(hypothesis));
    (edit_distance(&r, &h), r.len())
}

/// A reference word with its true speaker and time span in seconds.
#[derive(Debug, Clone)]
pub struct RefWord {
    pub speaker: usize,
    pub start: f32,
    pub end: f32,
    /// True when another speaker talks at the same time.
    pub overlap: bool,
}

/// A diarizer output span. `cluster` is the diarizer's own label, not a true speaker.
#[derive(Debug, Clone)]
pub struct Span {
    pub start: f32,
    pub end: f32,
    pub cluster: usize,
}

/// `(right, total)` words.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Tally {
    pub right: usize,
    pub total: usize,
}

impl Tally {
    pub fn share(self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.right as f64 / self.total as f64
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SpeakerAccuracy {
    pub outside_overlap: Tally,
    pub inside_overlap: Tally,
}

/// How a word gets its diarizer label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// One label: the cluster that covers the word most. Two words spoken together get one label each,
    /// so a perfect diarizer that marks both speakers still scores only about half inside overlaps.
    Strongest,
    /// Every cluster that covers at least half of the word. Right when the true speaker is among them.
    /// Extra labels cost nothing, so this is the generous bound.
    AnyCovering,
}

/// Clusters map to true speakers one to one, by the mapping that gets the most words right.
pub fn speaker_accuracy(
    words: &[RefWord],
    spans: &[Span],
    n_speakers: usize,
    rule: Rule,
) -> SpeakerAccuracy {
    let n_clusters = spans.iter().map(|s| s.cluster + 1).max().unwrap_or(0);
    let guesses: Vec<Vec<usize>> = words
        .iter()
        .map(|w| clusters_for(w, spans, n_clusters, rule))
        .collect();

    let mut counts = vec![vec![0usize; n_clusters]; n_speakers];
    for (w, g) in words.iter().zip(&guesses) {
        for c in g {
            counts[w.speaker][*c] += 1;
        }
    }
    let mapping = best_mapping(&counts, 0, &mut vec![false; n_clusters]).1;

    let mut acc = SpeakerAccuracy::default();
    for (w, g) in words.iter().zip(&guesses) {
        let tally = if w.overlap {
            &mut acc.inside_overlap
        } else {
            &mut acc.outside_overlap
        };
        tally.total += 1;
        tally.right += usize::from(mapping[w.speaker].is_some_and(|c| g.contains(&c)));
    }
    acc
}

fn clusters_for(w: &RefWord, spans: &[Span], n_clusters: usize, rule: Rule) -> Vec<usize> {
    let mut covered = vec![0f32; n_clusters];
    for s in spans {
        covered[s.cluster] += (w.end.min(s.end) - w.start.max(s.start)).max(0.0);
    }
    match rule {
        Rule::Strongest => covered
            .iter()
            .enumerate()
            .filter(|(_, c)| **c > 0.0)
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
            .into_iter()
            .collect(),
        Rule::AnyCovering => (0..n_clusters)
            .filter(|&c| covered[c] > 0.0 && covered[c] >= (w.end - w.start) / 2.0)
            .collect(),
    }
}

/// Best one-to-one speaker -> cluster assignment. Exhaustive, fine for a handful of speakers.
fn best_mapping(
    counts: &[Vec<usize>],
    speaker: usize,
    used: &mut Vec<bool>,
) -> (usize, Vec<Option<usize>>) {
    if speaker == counts.len() {
        return (0, vec![]);
    }
    let (mut best, mut best_map) = best_mapping(counts, speaker + 1, used); // leave this speaker unmatched
    best_map.insert(0, None);
    for c in 0..used.len() {
        if used[c] {
            continue;
        }
        used[c] = true;
        let (rest, mut map) = best_mapping(counts, speaker + 1, used);
        used[c] = false;
        if rest + counts[speaker][c] > best {
            best = rest + counts[speaker][c];
            map.insert(0, Some(c));
            best_map = map;
        }
    }
    (best, best_map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wer_counts_each_error_kind() {
        assert_eq!(wer("The cat sat.", "the cat sat"), (0, 3));
        assert_eq!(wer("the cat sat down", "the bat sat"), (2, 4)); // one substitution, one deletion
        assert_eq!(wer("don't stop", "don’t stop now"), (1, 2)); // curly apostrophe is the same word, one insertion
    }

    #[test]
    fn speaker_accuracy_maps_clusters_and_splits_overlap() {
        let w = |speaker, start, end, overlap| RefWord {
            speaker,
            start,
            end,
            overlap,
        };
        let words = [
            w(0, 0.0, 1.0, false),
            w(1, 2.0, 3.0, false),
            w(0, 4.0, 5.0, true),
            w(1, 4.0, 5.0, true),
        ];
        // Cluster labels are swapped against the true speakers; both clusters are marked over the overlap.
        let spans = [
            Span {
                start: 0.0,
                end: 1.5,
                cluster: 1,
            },
            Span {
                start: 1.5,
                end: 3.5,
                cluster: 0,
            },
            Span {
                start: 3.5,
                end: 5.0,
                cluster: 1,
            },
            Span {
                start: 4.2,
                end: 5.0,
                cluster: 0,
            },
        ];
        let any = speaker_accuracy(&words, &spans, 2, Rule::AnyCovering);
        assert_eq!(any.outside_overlap, Tally { right: 2, total: 2 });
        assert_eq!(any.inside_overlap, Tally { right: 2, total: 2 });
        // One label per word: both overlapped words get the longer cluster 1 span, so only speaker 0's word is right.
        let strongest = speaker_accuracy(&words, &spans, 2, Rule::Strongest);
        assert_eq!(strongest.outside_overlap, Tally { right: 2, total: 2 });
        assert_eq!(strongest.inside_overlap, Tally { right: 1, total: 2 });
    }
}
