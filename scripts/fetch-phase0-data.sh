#!/usr/bin/env bash
# Downloads the Phase 0 speech-to-text models and recordings to a path outside the repo.
# Every file has a pinned URL and sha256. None needs an account, a gated download or a payment.
# Usage: scripts/fetch-phase0-data.sh [DIR]   (default: $PARLEY_DATA or ~/.cache/parley-phase0)
set -euo pipefail

DATA="${1:-${PARLEY_DATA:-$HOME/.cache/parley-phase0}}"
GH=https://github.com/k2-fsa/sherpa-onnx/releases/download

# sha256  url  kind   (kind: file = copy to models/, models = tar.bz2 unpacked into models/, data = tar.gz unpacked into DATA)
ASSETS="
9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6 $GH/asr-models/silero_vad.onnx file
1495e91b59d7fc53ddca91f3431758ec99f8fa253fdb0e1e851be0f72b14b0f9 $GH/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2 models
b11acbbcd660b44a8e0df33724feb5aaa709cf65668f2823d59f656312544f22 $GH/asr-models/sherpa-onnx-whisper-turbo.tar.bz2 models
24615ee884c897d9d2ba09bb4d30da6bb1b15e685065962db5b02e76e4996488 $GH/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2 models
e9848563da86f263117134dfd7ad63c92355b37de492b55e325400c9d9c39012 $GH/speaker-recongition-models/wespeaker_en_voxceleb_resnet34_LM.onnx file
ad4a1802485d8b34c722d2a9d04249662f2ece5d28a7a039063ca22f515a789e $GH/speaker-recongition-models/nemo_en_titanet_small.onnx file
39fde525e59672dc6d1551919b1478f724438a95aa55f874b576be21967e6c23 https://www.openslr.org/resources/12/test-clean.tar.gz data
"

mkdir -p "$DATA/dl" "$DATA/models"
while read -r sha url kind; do
  [ -n "$sha" ] || continue
  f="$DATA/dl/$(basename "$url")"
  stamp="$f.done"
  [ -e "$stamp" ] && continue
  if ! { [ -e "$f" ] && echo "$sha  $f" | sha256sum -c --quiet -; }; then
    echo "download $url"
    curl -fL --retry 3 -o "$f" "$url"
    echo "$sha  $f" | sha256sum -c --quiet -
  fi
  case "$kind" in
    file) cp "$f" "$DATA/models/" ;;
    models) tar -xjf "$f" -C "$DATA/models" ;;
    data) tar -xzf "$f" -C "$DATA" ;;
  esac
  touch "$stamp"
done <<< "$ASSETS"
echo "ready: $DATA"
