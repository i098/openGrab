//! Phase 0 speech-to-text bench.
//!
//!   stt-bench stt      [--data DIR] [--threads 4,8,16] [--speakers 8] [--per-speaker 12]
//!   stt-bench diarize  [--data DIR] [--threads 16] [--mixes 3] [--turns 80]
//!
//! Data and models come from `scripts/fetch-phase0-data.sh` (default `~/.cache/parley-phase0`).

use parley_stt::score::{self, RefWord, Rule, Span, Tally};
use parley_stt::{
    Decoded, Engine, Recognizer, SAMPLE_RATE, SherpaTranscriber, Transcriber, VadConfig, diarize,
};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Instant;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

const SR: f32 = SAMPLE_RATE as f32;
const CHUNK: usize = 1600; // 100 ms, the size of the audio frames the join bot will deliver (INFERENCE)
const GAP_S: f32 = 0.6; // silence between utterances in the STT recording
const DELAY_LIMIT_S: f64 = 2.0;
const SHARE_LIMIT: f64 = 0.95;

fn main() -> Res<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let data = PathBuf::from(opt(&args, "--data", default_data()));
    match args.first().map(String::as_str) {
        Some("stt") => cmd_stt(&args, &data),
        Some("diarize") => cmd_diarize(&args, &data),
        _ => Err("usage: stt-bench <stt|diarize> [options]".into()),
    }
}

/// Host load average (1, 5, 15 min). The host is shared, so every latency number needs it next to it.
fn load() -> String {
    std::fs::read_to_string("/proc/loadavg")
        .map(|s| s.split(' ').take(3).collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
}

fn default_data() -> String {
    std::env::var("PARLEY_DATA").unwrap_or_else(|_| {
        format!(
            "{}/.cache/parley-phase0",
            std::env::var("HOME").unwrap_or_default()
        )
    })
}

fn opt<T: FromStr>(args: &[String], name: &str, default: T) -> T {
    args.windows(2)
        .find(|w| w[0] == name)
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(default)
}

// ---------- data ----------

struct Utt {
    id: String,
    path: PathBuf,
    text: String,
}

/// `(speaker id, utterances)` of LibriSpeech test-clean, sorted.
fn speakers(data: &Path) -> Res<Vec<(String, Vec<Utt>)>> {
    let root = data.join("LibriSpeech/test-clean");
    let sorted_dirs = |p: &Path| -> Res<Vec<PathBuf>> {
        let mut v: Vec<_> = std::fs::read_dir(p)?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|e| e.path())
            .collect();
        v.sort();
        Ok(v)
    };
    let mut out = Vec::new();
    for spk in sorted_dirs(&root)
        .map_err(|e| format!("{}: {e}; run scripts/fetch-phase0-data.sh", root.display()))?
    {
        let mut utts = Vec::new();
        for chap in sorted_dirs(&spk)? {
            let (s, c) = (name(&spk), name(&chap));
            for line in std::fs::read_to_string(chap.join(format!("{s}-{c}.trans.txt")))?.lines() {
                let (id, text) = line.split_once(' ').ok_or("bad transcript line")?;
                utts.push(Utt {
                    id: id.into(),
                    path: chap.join(format!("{id}.flac")),
                    text: text.into(),
                });
            }
        }
        utts.sort_by(|a, b| a.id.cmp(&b.id));
        out.push((name(&spk), utts));
    }
    Ok(out)
}

fn name(p: &Path) -> String {
    p.file_name().unwrap().to_string_lossy().into_owned()
}

fn read_flac(path: &Path) -> Res<Vec<f32>> {
    let mut r = claxon::FlacReader::open(path)?;
    let info = r.streaminfo();
    if info.sample_rate != SAMPLE_RATE || info.channels != 1 || info.bits_per_sample != 16 {
        return Err(format!("{}: expected 16 kHz mono 16-bit", path.display()).into());
    }
    Ok(r.samples()
        .map(|s| s.map(|s| s as f32 / 32768.0))
        .collect::<Result<_, _>>()?)
}

fn to_i16(pcm: &[f32]) -> Vec<i16> {
    pcm.iter()
        .map(|s| (s * 32768.0).clamp(-32768.0, 32767.0) as i16)
        .collect()
}

// ---------- stt ----------

struct Run {
    engine: Engine,
    threads: i32,
    errors: usize,
    ref_words: usize,
    digit_words: usize,
    segments: usize,
    delay: Vec<f64>,  // seconds from segment end to text
    decode: Vec<f64>, // seconds spent inside push() for segments that closed
}

fn cmd_stt(args: &[String], data: &Path) -> Res<()> {
    let (n_spk, per_spk): (usize, usize) =
        (opt(args, "--speakers", 8), opt(args, "--per-speaker", 12));
    let threads: Vec<i32> = opt(args, "--threads", "4,8,16".to_string())
        .split(',')
        .map(|t| t.parse().unwrap())
        .collect();

    let mut pcm = vec![0f32; SAMPLE_RATE as usize]; // 1 s lead-in
    let mut reference = Vec::new();
    let mut n_utts = 0;
    for (_, utts) in speakers(data)?.into_iter().take(n_spk) {
        for u in utts.into_iter().take(per_spk) {
            pcm.extend(read_flac(&u.path)?);
            pcm.extend(vec![0f32; (GAP_S * SR) as usize]);
            reference.push(u.text);
            n_utts += 1;
        }
    }
    pcm.extend(vec![0f32; SAMPLE_RATE as usize]); // lets the VAD close the last segment
    let (pcm, reference) = (to_i16(&pcm), reference.join(" "));
    println!(
        "sherpa-onnx {} ({}), host load average at start: {}",
        sherpa_onnx::version(),
        sherpa_onnx::git_sha1(),
        load()
    );
    println!(
        "recording: LibriSpeech test-clean, {n_spk} speakers x {per_spk} utterances = {n_utts} utterances, {:.1} s, {} reference words",
        pcm.len() as f32 / SR,
        score::normalize(&reference).len()
    );

    let dump: Option<PathBuf> = args
        .windows(2)
        .find(|w| w[0] == "--dump")
        .map(|w| PathBuf::from(&w[1]));
    if let Some(dir) = &dump {
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join("reference.txt"), &reference)?;
    }
    let mut runs = Vec::new();
    for t in &threads {
        for engine in [Engine::Parakeet, Engine::Whisper] {
            runs.push(run_stt(
                engine,
                *t,
                data,
                &pcm,
                &reference,
                dump.as_deref(),
            )?);
        }
    }

    println!(
        "\n| engine | threads | segments | WER | delay p50 | delay p95 | delay max | share <= {DELAY_LIMIT_S} s | decode p50 | decode p95 |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|");
    for r in &runs {
        println!(
            "| {} | {} | {} | {:.2}% ({}/{}) | {:.2} s | {:.2} s | {:.2} s | {:.1}% | {:.2} s | {:.2} s |",
            r.engine.name(),
            r.threads,
            r.segments,
            100.0 * r.errors as f64 / r.ref_words as f64,
            r.errors,
            r.ref_words,
            pct(&r.delay, 0.5),
            pct(&r.delay, 0.95),
            pct(&r.delay, 1.0),
            100.0 * share_within(&r.delay),
            pct(&r.decode, 0.5),
            pct(&r.decode, 0.95),
        );
    }
    println!(
        "\nhypothesis words with a digit (not normalized to spoken form): {}",
        runs.iter()
            .map(|r| format!("{} t{}: {}", r.engine.name(), r.threads, r.digit_words))
            .collect::<Vec<_>>()
            .join(", ")
    );
    for t in &threads {
        let pick = runs
            .iter()
            .filter(|r| r.threads == *t && share_within(&r.delay) >= SHARE_LIMIT)
            .min_by(|a, b| (a.errors).cmp(&b.errors));
        match pick {
            Some(r) => println!("pick at {t} threads: {}", r.engine.name()),
            None => println!(
                "pick at {t} threads: none, no engine gives text in {DELAY_LIMIT_S} s for {:.0}% of segments",
                SHARE_LIMIT * 100.0
            ),
        }
    }
    println!("host load average at end: {}", load());
    Ok(())
}

/// Replays the recording at real-time pace on a virtual clock: a decode that outlasts the audio it
/// blocks delays every later chunk, as it would on one worker thread with a buffering input.
fn run_stt(
    engine: Engine,
    threads: i32,
    data: &Path,
    pcm: &[i16],
    reference: &str,
    dump: Option<&Path>,
) -> Res<Run> {
    let dir = match engine {
        Engine::Parakeet => "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8",
        Engine::Whisper => "sherpa-onnx-whisper-turbo",
    };
    let m = data.join("models");
    let loaded = Instant::now();
    let mut tr = SherpaTranscriber::new(
        engine,
        &m.join(dir),
        &m.join("silero_vad.onnx"),
        &VadConfig::default(),
        threads,
    )?;
    eprintln!(
        "{} t{threads}: loaded in {:.1} s",
        engine.name(),
        loaded.elapsed().as_secs_f32()
    );

    let (mut clock, mut texts, mut delay, mut decode) = (0f64, Vec::new(), Vec::new(), Vec::new());
    let mut lines = Vec::new();
    for (i, chunk) in pcm.chunks(CHUNK).enumerate() {
        clock = clock.max(((i * CHUNK + chunk.len()) as f64) / f64::from(SR));
        let t0 = Instant::now();
        let segs = tr.push(chunk)?;
        let spent = t0.elapsed().as_secs_f64();
        clock += spent;
        for s in segs {
            delay.push(clock - s.end_ms as f64 / 1000.0);
            decode.push(spent);
            lines.push(format!(
                "{:.2}\t{:.2}\t{}",
                s.start_ms as f64 / 1000.0,
                s.end_ms as f64 / 1000.0,
                s.text
            ));
            texts.push(s.text);
        }
    }
    if let Some(dir) = dump {
        std::fs::create_dir_all(dir)?;
        std::fs::write(
            dir.join(format!("{}-t{threads}.txt", engine.name())),
            lines.join("\n"),
        )?;
    }
    let hyp = texts.join(" ");
    let (errors, ref_words) = score::wer(reference, &hyp);
    Ok(Run {
        engine,
        threads,
        errors,
        ref_words,
        digit_words: score::normalize(&hyp)
            .iter()
            .filter(|w| w.chars().any(|c| c.is_ascii_digit()))
            .count(),
        segments: texts.len(),
        delay,
        decode,
    })
}

fn pct(v: &[f64], q: f64) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    s.get(((s.len() as f64 * q).ceil() as usize).saturating_sub(1))
        .copied()
        .unwrap_or(f64::NAN)
}

fn share_within(v: &[f64]) -> f64 {
    v.iter().filter(|d| **d <= DELAY_LIMIT_S).count() as f64 / v.len().max(1) as f64
}

// ---------- diarization ----------

struct Placed {
    speaker: usize,
    span: (f32, f32),
    words: Vec<(f32, f32)>,
}

fn cmd_diarize(args: &[String], data: &Path) -> Res<()> {
    let (threads, mixes, turns): (i32, usize, usize) = (
        opt(args, "--threads", 16),
        opt(args, "--mixes", 3),
        opt(args, "--turns", 80),
    );
    let m = data.join("models");
    let seg_model = m.join("sherpa-onnx-pyannote-segmentation-3-0/model.onnx");
    let embeddings = [
        "wespeaker_en_voxceleb_resnet34_LM.onnx",
        "nemo_en_titanet_small.onnx",
    ];
    let rec = Recognizer::new(
        Engine::Parakeet,
        &m.join("sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8"),
        threads,
    )?;
    let all = speakers(data)?;

    let rules = [
        (Rule::Strongest, "one label per word"),
        (Rule::AnyCovering, "any covering label"),
    ];
    let mut total = vec![[score::SpeakerAccuracy::default(); 2]; embeddings.len()];
    for k in 0..mixes {
        let pools: Vec<&[Utt]> = all[3 * k..3 * k + 3]
            .iter()
            .map(|(_, u)| u.as_slice())
            .collect();
        let (pcm, words, passages) = build_mix(&rec, &pools, turns, 7 + k as u64)?;
        let inside = words.iter().filter(|w| w.overlap).count();
        println!(
            "\nmix {k}: speakers {}, {:.0} s, {} words, {passages} passages with two speakers at once (>= 1 s), {inside} words inside overlaps",
            all[3 * k..3 * k + 3]
                .iter()
                .map(|(s, _)| s.as_str())
                .collect::<Vec<_>>()
                .join("+"),
            pcm.len() as f32 / SR,
            words.len()
        );
        for (e, emb) in embeddings.iter().enumerate() {
            let t0 = Instant::now();
            let segs = diarize(&pcm, &seg_model, &m.join(emb), 3, threads)?;
            let spans: Vec<Span> = segs
                .iter()
                .map(|s| Span {
                    start: s.start,
                    end: s.end,
                    cluster: s.speaker,
                })
                .collect();
            println!("  {emb} ({:.1} s)", t0.elapsed().as_secs_f32());
            for (r, (rule, label)) in rules.iter().enumerate() {
                let acc = score::speaker_accuracy(&words, &spans, 3, *rule);
                println!(
                    "    {label}: outside {}  inside {}",
                    show(acc.outside_overlap),
                    show(acc.inside_overlap)
                );
                total[e][r].outside_overlap = add(total[e][r].outside_overlap, acc.outside_overlap);
                total[e][r].inside_overlap = add(total[e][r].inside_overlap, acc.inside_overlap);
            }
        }
    }
    println!("\nall {mixes} mixes, num_speakers = 3 given (limits: outside >= 95%, inside >= 80%)");
    for (e, emb) in embeddings.iter().enumerate() {
        println!("  {emb}");
        for (r, (_, label)) in rules.iter().enumerate() {
            println!(
                "    {label}: outside {}  inside {}",
                show(total[e][r].outside_overlap),
                show(total[e][r].inside_overlap)
            );
        }
    }
    Ok(())
}

fn show(t: Tally) -> String {
    format!("{:.1}% ({}/{})", 100.0 * t.share(), t.right, t.total)
}

fn add(a: Tally, b: Tally) -> Tally {
    Tally {
        right: a.right + b.right,
        total: a.total + b.total,
    }
}

struct Lcg(u64);

impl Lcg {
    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        lo + (hi - lo) * ((self.0 >> 40) as f32 / (1u64 << 24) as f32)
    }
    fn below(&mut self, n: usize) -> usize {
        self.range(0.0, n as f32) as usize % n
    }
}

/// A 3-speaker call: every third turn starts 1.5 to 3 s before the previous speaker ends.
/// Word times come from Parakeet run on each clean utterance before mixing.
fn build_mix(
    rec: &Recognizer,
    pools: &[&[Utt]],
    turns: usize,
    seed: u64,
) -> Res<(Vec<f32>, Vec<RefWord>, usize)> {
    let mut rng = Lcg(seed);
    let mut next = vec![0usize; pools.len()];
    let mut mix = vec![0f32; SAMPLE_RATE as usize];
    let mut placed: Vec<Placed> = Vec::new();

    for k in 0..turns {
        let overlap = k % 3 == 2;
        let mut speaker = rng.below(pools.len());
        if placed.last().is_some_and(|p| p.speaker == speaker) {
            speaker = (speaker + 1) % pools.len();
        }
        // Next utterance of this speaker with at least 4.5 s of audio, so an overlap never swallows a whole turn.
        let (mut audio, mut words);
        loop {
            let u = pools[speaker]
                .get(next[speaker])
                .ok_or("speaker pool exhausted; lower --turns")?;
            next[speaker] += 1;
            audio = read_flac(&u.path)?;
            if audio.len() < (4.5 * SR) as usize {
                continue;
            }
            normalize_rms(&mut audio, 0.06);
            words = words_of(&rec.decode(&audio)?);
            if words.len() >= 4 {
                break;
            }
        }
        let rel = (words[0].0, words[words.len() - 1].1);
        let start = match placed.last() {
            None => 1.0,
            Some(last) if overlap => {
                let floor = placed
                    .len()
                    .checked_sub(2)
                    .map_or(0.0, |i| placed[i].span.1 + 0.2)
                    .max(last.span.0 + 1.0);
                (last.span.1 - rng.range(1.5, 3.0)).max(floor)
            }
            Some(_) => placed.iter().map(|p| p.span.1).fold(0.0, f32::max) + rng.range(0.4, 1.0),
        };
        let offset = ((start - rel.0) * SR).max(0.0) as usize;
        if mix.len() < offset + audio.len() {
            mix.resize(offset + audio.len(), 0.0);
        }
        mix.iter_mut()
            .skip(offset)
            .zip(&audio)
            .for_each(|(m, a)| *m += a);
        let shift = offset as f32 / SR;
        placed.push(Placed {
            speaker,
            span: (rel.0 + shift, rel.1 + shift),
            words: words.iter().map(|w| (w.0 + shift, w.1 + shift)).collect(),
        });
    }
    mix.extend(vec![0f32; SAMPLE_RATE as usize]);

    let mut words = Vec::new();
    for (i, p) in placed.iter().enumerate() {
        for &(start, end) in &p.words {
            let mid = (start + end) / 2.0;
            let overlap = placed.iter().enumerate().any(|(j, q)| {
                j != i && q.speaker != p.speaker && q.span.0 <= mid && mid <= q.span.1
            });
            words.push(RefWord {
                speaker: p.speaker,
                start,
                end,
                overlap,
            });
        }
    }
    let passages = (0..placed.len())
        .flat_map(|i| (i + 1..placed.len()).map(move |j| (i, j)))
        .filter(|&(i, j)| {
            placed[i].speaker != placed[j].speaker
                && placed[i].span.1.min(placed[j].span.1) - placed[i].span.0.max(placed[j].span.0)
                    >= 1.0
        })
        .count();
    Ok((mix, words, passages))
}

fn normalize_rms(pcm: &mut [f32], target: f32) {
    let rms = (pcm.iter().map(|s| s * s).sum::<f32>() / pcm.len() as f32).sqrt();
    pcm.iter_mut().for_each(|s| *s *= target / rms);
}

/// Words with `(start, end)` seconds from the token timestamps; a token starting with a space begins a word.
/// Punctuation tokens do not stretch a word.
fn words_of(d: &Decoded) -> Vec<(f32, f32)> {
    let (Some(ts), tokens) = (&d.timestamps, &d.tokens) else {
        return Vec::new();
    };
    let mut words: Vec<(f32, f32)> = Vec::new();
    for (i, tok) in tokens.iter().enumerate() {
        let end = ts[i] + d.durations.as_ref().map_or(0.08, |du| du[i]);
        match words.last_mut() {
            Some(w) if !tok.starts_with(' ') => {
                if tok.chars().any(char::is_alphanumeric) {
                    w.1 = end;
                }
            }
            _ => words.push((ts[i], end)),
        }
    }
    words
}
