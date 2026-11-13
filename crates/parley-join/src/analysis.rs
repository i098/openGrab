//! Pure helpers for the exit test: audio levels, the speaking-indicator timeline, WAV output.

use crate::protocol::SAMPLE_RATE;

/// Root-mean-square of 16-bit samples, in sample units (0 for an empty slice).
pub fn rms(pcm: &[i16]) -> f64 {
    if pcm.is_empty() {
        return 0.0;
    }
    (pcm.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / pcm.len() as f64).sqrt()
}

/// One entry per change of the set of people the page shows as speaking: `(time ms, sorted names)`.
pub type SpeakingTimeline = [(u64, Vec<String>)];

/// Share of 250 ms samples in `[start, end)` where exactly `expected` (sorted) was lit.
/// The set in force at a sample is the latest change at or before it; before the first change it is empty.
pub fn speaking_match(tl: &SpeakingTimeline, start: u64, end: u64, expected: &[String]) -> f64 {
    let (mut hits, mut n) = (0u32, 0u32);
    let mut t = start;
    while t < end {
        let lit = tl
            .iter()
            .rev()
            .find(|(at, _)| *at <= t)
            .map_or(&[][..], |(_, names)| names.as_slice());
        hits += u32::from(lit == expected);
        n += 1;
        t += 250;
    }
    if n == 0 {
        0.0
    } else {
        f64::from(hits) / f64::from(n)
    }
}

/// A mono 16 kHz 16-bit WAV file.
pub fn wav_bytes(pcm: &[i16]) -> Vec<u8> {
    let data = (pcm.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + data as usize);
    b.extend(b"RIFF");
    b.extend((36 + data).to_le_bytes());
    b.extend(b"WAVEfmt ");
    b.extend(16u32.to_le_bytes());
    b.extend(1u16.to_le_bytes()); // PCM
    b.extend(1u16.to_le_bytes()); // mono
    b.extend(SAMPLE_RATE.to_le_bytes());
    b.extend((SAMPLE_RATE * 2).to_le_bytes());
    b.extend(2u16.to_le_bytes());
    b.extend(16u16.to_le_bytes());
    b.extend(b"data");
    b.extend(data.to_le_bytes());
    b.extend(pcm.iter().flat_map(|s| s.to_le_bytes()));
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn rms_of_a_square_wave_is_its_amplitude() {
        assert_eq!(rms(&[100, -100, 100, -100]), 100.0);
        assert_eq!(rms(&[]), 0.0);
    }

    #[test]
    fn speaking_match_follows_the_latest_change_and_requires_the_exact_set() {
        let tl = vec![
            (1_000, names(&["A"])),
            (2_000, names(&["A", "B"])),
            (3_000, vec![]),
        ];
        // 1000..2000 -> A for 4 samples; 2000..3000 -> A+B for 4 samples.
        assert_eq!(speaking_match(&tl, 1_000, 2_000, &names(&["A"])), 1.0);
        assert_eq!(speaking_match(&tl, 1_000, 3_000, &names(&["A"])), 0.5);
        assert_eq!(
            speaking_match(&tl, 0, 1_000, &names(&["A"])),
            0.0,
            "nobody lit before the first change"
        );
        assert_eq!(
            speaking_match(&tl, 5, 5, &names(&["A"])),
            0.0,
            "empty window"
        );
    }

    #[test]
    fn wav_header_sizes_match_the_data() {
        let w = wav_bytes(&[1, 2, 3]);
        assert_eq!(w.len(), 44 + 6);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(w[40..44].try_into().unwrap()), 6);
        assert_eq!(u32::from_le_bytes(w[4..8].try_into().unwrap()), 36 + 6);
    }
}
