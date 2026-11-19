"""Qwen3-TTS voice-clone worker (faster-qwen3-tts, NVIDIA GPU). Protocol and pacing: see common.py.

A voice is a folder in <InkVoice data>/voices/<name>/ holding
  ref.wav  a clean 10-25 s sample of the voice
  ref.txt  the exact words spoken in ref.wav
Every piece is cloned from that sample, so the reader stays the same for the whole book.

  --model        0.6B (faster, ~3 GB of GPU memory) or 1.7B (closer to the sample, ~5 GB)
  --temperature  sampling temperature (default 0.9; lower = steadier, higher = livelier)

Word timings are measured by forced alignment (aligner.py), so the highlight follows the voice.
"""
import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import common

ap = common.base_args("narrator")
ap.add_argument("--model", choices=["0.6B", "1.7B"], default="0.6B")
ap.add_argument("--temperature", type=float, default=0.9)
# shorter pieces than the small engines: long clones drift, and each piece must fit the model's window
ap.set_defaults(max_chars=400)
args = ap.parse_args()


def load():
    import numpy as np
    import torch
    if not torch.cuda.is_available():
        raise RuntimeError("Qwen3 needs an NVIDIA GPU (CUDA was not found)")
    vdir = os.path.join(common.data_dir(), "voices", args.voice.lower())
    ref, ref_txt = os.path.join(vdir, "ref.wav"), os.path.join(vdir, "ref.txt")
    if not (os.path.isfile(ref) and os.path.isfile(ref_txt)):
        raise ValueError(f"voice '{args.voice}' not found: put ref.wav and ref.txt in {vdir}")
    with open(ref_txt, encoding="utf-8") as f:
        ref_text = f.read().strip()

    from faster_qwen3_tts import FasterQwen3TTS
    tts = FasterQwen3TTS.from_pretrained(f"Qwen/Qwen3-TTS-12Hz-{args.model}-Base")

    def synth(text):
        wavs, _sr = tts.generate_voice_clone(
            text=text, language="English", ref_audio=ref, ref_text=ref_text, temperature=args.temperature)
        return np.asarray(wavs[0], dtype=np.float32).reshape(-1)

    # measured word timings; if the aligner cannot load, common.py falls back to estimating
    align_fn = None
    try:
        from aligner import CtcAligner
        align_fn = CtcAligner("cuda").align
    except Exception as e:
        print(f"word aligner unavailable, timings will be estimated: {e}", file=sys.stderr)

    return synth, 24000, align_fn


common.serve(args, load)
