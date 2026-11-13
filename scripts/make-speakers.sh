#!/usr/bin/env bash
# Make three known speech recordings with reference text for the fake-microphone test.
# Usage: scripts/make-speakers.sh <out-dir>   (needs espeak-ng and ffmpeg)
set -euo pipefail
out=${1:?usage: make-speakers.sh <out-dir>}
mkdir -p "$out"
make() { # <letter> <espeak voice> <text>
  printf '%s\n' "$3" > "$out/$1.txt"
  espeak-ng -v "$2" -s 150 -w "$out/$1.raw.wav" "$3"
  # Chromium's fake capture device reads 16-bit PCM WAV; it loops the file.
  ffmpeg -loglevel error -y -i "$out/$1.raw.wav" -ar 48000 -ac 1 -c:a pcm_s16le "$out/$1.wav"
  rm "$out/$1.raw.wav"
}
make a en-us+m3 "The quarterly review starts with the budget. Marketing asked for two more hires, and engineering wants to move the launch to the fifteenth of March."
make b en-gb+f2 "I disagree with moving the launch. The customer pilot ends on the first of March, so we should ship the first version before that date."
make c en-us+m7 "Let us record two decisions. First, the launch stays on the first of March. Second, finance will review the hiring request by Friday."
