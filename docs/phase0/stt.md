# Phase 0: speech-to-text and speaker names

This document records the speech-to-text part of Phase 0 (plan sections 6 and 6.1).
Every number below comes from a run on this host.
The raw output is pasted without edits.

## Result

The engine pick is Parakeet TDT 0.6b v3.
At 8 and 16 threads it gives text in 2 seconds or less for 99.1% and 100.0% of segments.
Whisper large-v3-turbo gives text in 2 seconds or less for 5.9% and 7.6% of segments at the same thread counts.
Parakeet also has the lower word error rate: 4.29% against 7.82% on the same recording.
At 4 threads no engine passes: Parakeet reaches 87.6%.
Local models only (Q3 a): no API, no spend.

The speaker-name fallback (sherpa-onnx diarization on mixed audio) passes the outside-overlap limit and fails the inside-overlap limit in the strict reading.
With the `nemo_en_titanet_small` embedding model it labels 98.2% of the words outside overlaps correctly (limit 95%).
Inside overlaps it gets 94.8% if a word may carry every speaker the diarizer marks for it, and 48.1% if a word gets one speaker (limit 80%).
A transcript needs one name per word, so the strict figure is the one that matters.
A diarizer that marks both speakers perfectly still scores about 50% in that reading.
R13 therefore stays open for overlapping speech until the track-matching method is measured on a live call.

## Commands

```sh
scripts/fetch-phase0-data.sh                                         # models and LibriSpeech to ~/.cache/parley-phase0 (or $PARLEY_DATA, or the first argument)
cargo build --release
cargo test --release -p parley-stt                                   # word error rate and speaker scoring, no model files
cargo run --release -p parley-stt --bin stt-bench -- stt             # both engines, threads 4,8,16
cargo run --release -p parley-stt --bin stt-bench -- diarize         # 3 mixes, 2 embedding models
```

Options: `stt --speakers 8 --per-speaker 12 --threads 4,8,16 --dump DIR` and `diarize --mixes 3 --turns 80 --threads 16`.
`--dump DIR` writes the reference text and each engine's segments with times.
The fetch script pins every URL by sha256.
It downloads no file that needs an account, a gated download or a payment.
The first `cargo build` downloads the prebuilt sherpa-onnx static libraries from its GitHub release (build script of `sherpa-onnx-sys`).
`blaze build` fails on this host: it cannot compile the build script of `sherpa-onnx-sys` (error `unsupported: LLVM intrinsic x86.aesni.aeskeygenassist` in an `aes` dependency).
All numbers here come from plain `cargo build --release` binaries.

## Engine library: sherpa-onnx

`crates/parley-stt` uses the `sherpa-onnx` 2.0.1 crate (Apache-2.0, native library sherpa-onnx af2d8d81).
One library gives Silero VAD, the Parakeet and Whisper recognizers, and speaker diarization.
`parakeet-rs` gives Parakeet and its own Sortformer diarizer, but not Whisper and not the sherpa-onnx diarization that the plan names.
Two stacks would be needed for the same scope.
I did not build `parakeet-rs` on this host.
The pick rests on scope, and on the fact that sherpa-onnx builds and runs here (Intel Xeon Platinum 8160, 96 threads, no GPU).

Models, all as ONNX files:

| Part | File | License |
|---|---|---|
| VAD | `silero_vad.onnx` | MIT |
| Parakeet TDT 0.6b v3 | `sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8` (int8) | CC-BY-4.0 |
| Whisper large-v3-turbo | `sherpa-onnx-whisper-turbo` (int8; `turbo` is the 809 M parameter large-v3-turbo of openai/whisper) | MIT |
| Segmentation | `sherpa-onnx-pyannote-segmentation-3-0` | MIT (Hugging Face card; the sherpa-onnx release copy needs no account) |
| Speaker embedding | `nemo_en_titanet_small.onnx`, `wespeaker_en_voxceleb_resnet34_LM.onnx` | not checked |

Both ASR models run as int8 files.
Full-precision files were not measured.

## Method: speech-to-text

Recording: LibriSpeech test-clean (CC-BY-4.0), the first 12 utterances of each of the first 8 speakers in sorted order.
That is 96 utterances, 838.1 s of audio, 2097 reference words.
Each utterance is followed by 0.6 s of digital silence, with 1 s of silence at the start and at the end.
Both engines see the same samples.

Pipeline: `SherpaTranscriber` (the `Transcriber` trait).
Silero VAD uses threshold 0.5, minimum silence 0.4 s, minimum speech 0.25 s, and a cut after 20 s of speech.
Each closed segment goes to one engine, with greedy search.
Whisper runs with `language = en`.
The bench feeds 100 ms chunks.

Delay: seconds from the end of the segment (as the VAD reports it) to the moment the text exists.
The bench replays the recording on a virtual clock at real-time pace.
A chunk arrives when its audio would arrive.
A `push` that decodes a segment blocks the later chunks, so a slow engine adds a queue delay to the next segments.
The delay includes the 0.4 s silence wait of the VAD.
For Parakeet at 16 threads the median delay is 0.75 s and the median decode time is 0.33 s.
"Decode" is the time spent inside the `push` call that closed the segment.

Word error rate: one edit distance over the whole recording.
Both texts are upper-cased, and everything except letters, digits and apostrophes splits words.
There is no spelling or number normalizer.
Absolute rates are higher than published ones because of spelling variants (`counselled` and `counseled`, `gray` and `grey`, `saint` and `st`) and digits.
Both engines meet the same variants, so the comparison between engines holds.
The count of hypothesis words that contain a digit is printed below the table.
Whisper has 4 such words of 2097, which is at most 0.2 points of its rate.

The pick rule is the one in plan section 6.1: the lower word error rate among the engines that give text in 2 seconds or less for 95% of the segments.

## Measured: speech-to-text

The host was shared with other jobs while this ran.
The load average (1, 5 and 15 minutes) was 129.37 160.66 228.19 at the start and 96.30 90.11 99.26 at the end, on 96 threads.
Latency numbers are for that load and are probably pessimistic for an idle host.
The word error rates do not depend on load.

Command: `stt-bench stt` (defaults).
Output, unedited:

```text
sherpa-onnx 2.0.1 (af2d8d81), host load average at start: 129.37 160.66 228.19
recording: LibriSpeech test-clean, 8 speakers x 12 utterances = 96 utterances, 838.1 s, 2097 reference words
parakeet-tdt-0.6b-v3 t4: loaded in 13.9 s
whisper-large-v3-turbo t4: loaded in 5.3 s
parakeet-tdt-0.6b-v3 t8: loaded in 5.6 s
whisper-large-v3-turbo t8: loaded in 4.0 s
parakeet-tdt-0.6b-v3 t16: loaded in 11.5 s
whisper-large-v3-turbo t16: loaded in 3.0 s

| engine | threads | segments | WER | delay p50 | delay p95 | delay max | share <= 2 s | decode p50 | decode p95 |
|---|---|---|---|---|---|---|---|---|---|
| parakeet-tdt-0.6b-v3 | 4 | 113 | 4.29% (90/2097) | 1.17 s | 2.48 s | 3.76 s | 87.6% | 0.76 s | 2.07 s |
| whisper-large-v3-turbo | 4 | 119 | 7.82% (164/2097) | 7.14 s | 13.86 s | 24.58 s | 0.0% | 4.73 s | 12.46 s |
| parakeet-tdt-0.6b-v3 | 8 | 113 | 4.29% (90/2097) | 0.79 s | 1.46 s | 2.08 s | 99.1% | 0.38 s | 1.20 s |
| whisper-large-v3-turbo | 8 | 119 | 7.82% (164/2097) | 4.00 s | 10.33 s | 23.03 s | 5.9% | 3.04 s | 9.00 s |
| parakeet-tdt-0.6b-v3 | 16 | 113 | 4.29% (90/2097) | 0.75 s | 1.32 s | 1.76 s | 100.0% | 0.33 s | 1.08 s |
| whisper-large-v3-turbo | 16 | 119 | 7.82% (164/2097) | 4.11 s | 13.27 s | 27.25 s | 7.6% | 3.05 s | 10.70 s |

hypothesis words with a digit (not normalized to spoken form): parakeet-tdt-0.6b-v3 t4: 0, whisper-large-v3-turbo t4: 4, parakeet-tdt-0.6b-v3 t8: 0, whisper-large-v3-turbo t8: 4, parakeet-tdt-0.6b-v3 t16: 0, whisper-large-v3-turbo t16: 4
pick at 4 threads: none, no engine gives text in 2 s for 95% of segments
pick at 8 threads: parakeet-tdt-0.6b-v3
pick at 16 threads: parakeet-tdt-0.6b-v3
host load average at end: 96.30 90.11 99.26
```

The word error rate is the same at every thread count, as expected.
Whisper does not get faster from 8 to 16 threads on this loaded host.
Its median decode time is about 3 s for a segment of a few seconds.
Its decode time p95 is 10.70 s at 16 threads.
A decode that lasts longer than the audio that follows queues the next segments, which probably explains the 27.25 s maximum (INFERENCE, not traced).
On a smaller run (`stt --speakers 2 --per-speaker 12 --threads 16`, 24 utterances, 631 words) both engines got 3.96% (25/631).
I did not find why Whisper is worse on the full recording.
The errors I looked at in the small run were spelling variants, digits and rare names, in both engines.

What this does not show:

- Only English was tested. Parakeet v3 and Whisper are multilingual, and neither was measured on other languages.
- Read audiobook speech, one speaker at a time. Meet audio has codec loss, noise and crosstalk.
- One worker thread per stream. Three or more streams that decode at once were not measured.
- No test on an idle host. The 8-thread Parakeet run is near the limit (maximum 2.08 s), so the margin is thin.

## Method: speaker names (diarization fallback)

Plan row "Speaker names": run the sherpa-onnx diarization (a segmentation model and a speaker-embedding model) on mixed audio.
The limits are 95% of the words outside overlaps and 80% of the words inside overlaps.

Mixes: 3 synthetic calls with 3 speakers each (test-clean speakers `1089+1188+121`, `1221+1284+1320`, `1580+1995+2094`).
Each mix has 80 turns.
Only utterances of 4.5 s or longer are used, so an overlap never covers a whole turn.
Every third turn starts 1.5 s to 3 s before the previous speaker ends, so two speakers talk at once.
Each utterance is scaled to the same RMS before mixing.
The diarizer gets `num_speakers = 3`, as a call roster would give.

Words and their times: Parakeet runs on each clean single-speaker utterance before mixing.
The words are the ones Parakeet hears, with token times, not the reference text.
A word is "inside an overlap" when its middle lies in the speech span of an utterance of a different speaker.
A passage is a pair of utterances of different speakers that overlap for 1 s or more.

Scoring has two rules, because a word in an overlap has two possible speakers:

- One label per word: the diarizer cluster that covers the word most. The best one-to-one map from clusters to speakers is used.
- Any covering label: the word counts as right when the true speaker's cluster covers at least half of the word. This is the generous reading, because extra labels cost nothing.

## Measured: speaker names

Command: `stt-bench diarize` (defaults, 16 threads).
Output, unedited:

```text
mix 0: speakers 1089+1188+121, 839 s, 2259 words, 26 passages with two speakers at once (>= 1 s), 325 words inside overlaps
  wespeaker_en_voxceleb_resnet34_LM.onnx (82.0 s)
    one label per word: outside 73.6% (1424/1934)  inside 34.5% (112/325)
    any covering label: outside 72.3% (1399/1934)  inside 64.6% (210/325)
  nemo_en_titanet_small.onnx (99.9 s)
    one label per word: outside 97.5% (1885/1934)  inside 50.5% (164/325)
    any covering label: outside 95.8% (1852/1934)  inside 93.8% (305/325)

mix 1: speakers 1221+1284+1320, 747 s, 2359 words, 26 passages with two speakers at once (>= 1 s), 371 words inside overlaps
  wespeaker_en_voxceleb_resnet34_LM.onnx (71.0 s)
    one label per word: outside 70.7% (1406/1988)  inside 33.2% (123/371)
    any covering label: outside 70.0% (1391/1988)  inside 63.6% (236/371)
  nemo_en_titanet_small.onnx (84.8 s)
    one label per word: outside 98.5% (1958/1988)  inside 45.8% (170/371)
    any covering label: outside 97.5% (1938/1988)  inside 96.0% (356/371)

mix 2: speakers 1580+1995+2094, 694 s, 2125 words, 26 passages with two speakers at once (>= 1 s), 365 words inside overlaps
  wespeaker_en_voxceleb_resnet34_LM.onnx (70.9 s)
    one label per word: outside 52.8% (929/1760)  inside 38.9% (142/365)
    any covering label: outside 51.1% (900/1760)  inside 71.5% (261/365)
  nemo_en_titanet_small.onnx (72.5 s)
    one label per word: outside 98.7% (1737/1760)  inside 48.2% (176/365)
    any covering label: outside 96.2% (1693/1760)  inside 94.5% (345/365)

all 3 mixes, num_speakers = 3 given (limits: outside >= 95%, inside >= 80%)
  wespeaker_en_voxceleb_resnet34_LM.onnx
    one label per word: outside 66.2% (3759/5682)  inside 35.5% (377/1061)
    any covering label: outside 64.9% (3690/5682)  inside 66.6% (707/1061)
  nemo_en_titanet_small.onnx
    one label per word: outside 98.2% (5580/5682)  inside 48.1% (510/1061)
    any covering label: outside 96.5% (5483/5682)  inside 94.8% (1006/1061)
```

Reading the numbers:

- Every mix has 26 passages with two speakers at once, so the plan's "at least 20" holds in each mix.
- Both embedding models were run on the same segmentation output. The `wespeaker` model fails both limits. The `titanet_small` model is the one to use.
- Pooled over 3 mixes, `titanet_small` passes the outside limit (98.2% and 96.5%, both above 95%).
- Inside overlaps it fails the 80% limit with one label per word (48.1%) and passes with any covering label (94.8%).
- The embedding choice was made on these same mixes, so the `titanet_small` figures are not held-out.

What this does not show:

- Synthetic mixes: no room echo, no codec, no network loss, equal speech levels. Real Meet audio is harder (INFERENCE).
- The ASR text of overlapped speech was not measured. Two voices at once give wrong words, whatever the speaker label is.
- The whole mix is diarized after the fact. A live call needs an online diarizer, which was not tested.
- The track-matching method was not tested. It needs a live call and belongs to the join-bot task.

## Attribution

- Speech-to-text model: NVIDIA Parakeet TDT 0.6b v3, <https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3>, licensed under CC-BY-4.0 (<https://creativecommons.org/licenses/by/4.0/>). The ONNX conversion used here is `sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8` by k2-fsa. The model was converted and quantized to int8.
- Recordings: LibriSpeech test-clean, V. Panayotov, G. Chen, D. Povey and S. Khudanpur, "Librispeech: an ASR corpus based on public domain audio books", ICASSP 2015, licensed under CC-BY-4.0, <https://www.openslr.org/12>. Used for measurement only. No audio is in the repository.
