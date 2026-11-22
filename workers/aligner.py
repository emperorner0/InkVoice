"""Measured word timings: forced alignment of the known text against the generated audio.

common.align() only estimates where each word falls (pauses matched to punctuation, the rest spread
by character count), which drifts by seconds on expressive voices. Engines that already ship PyTorch
can use this instead: a small wav2vec2 CTC model (facebook/wav2vec2-base-960h, ~360 MB) scores every
20 ms frame against the letters of the text, and the best monotonic path gives each word's start.
"""
import re
import numpy as np

MODEL = "facebook/wav2vec2-base-960h"
_DROP = re.compile(r"[^A-Z']")


class CtcAligner:
    def __init__(self, device="cuda"):
        import torch
        from transformers import Wav2Vec2ForCTC, Wav2Vec2Processor
        self.torch = torch
        self.device = device
        vocab = Wav2Vec2Processor.from_pretrained(MODEL).tokenizer.get_vocab()
        self.vocab, self.blank, self.sep = vocab, vocab["<pad>"], vocab["|"]
        self.model = Wav2Vec2ForCTC.from_pretrained(MODEL).to(device).eval()

    def _emissions(self, a, sr):
        torch = self.torch
        x = torch.as_tensor(np.asarray(a, dtype=np.float32), device=self.device)
        if sr != 16000:
            import torchaudio
            x = torchaudio.functional.resample(x, sr, 16000)
        out, win = [], 16000 * 20
        with torch.inference_mode():
            for i in range(0, len(x), win):
                seg = x[i:i + win + 400]
                if len(seg) < 800:
                    break
                seg = (seg - seg.mean()) / (seg.std() + 1e-7)
                lp = torch.log_softmax(self.model(seg[None]).logits[0].float(), -1).cpu().numpy()
                out.append(lp[: win // 320] if i + win < len(x) else lp)
        em = np.concatenate(out)
        return em, len(em) / (len(x) / 16000)

    def _path(self, em, tokens):
        """Frame at which each token is emitted on the best monotonic path."""
        T, N = em.shape[0], len(tokens)
        tok = np.array(tokens)
        tr = np.full((T + 1, N + 1), -1e9, dtype=np.float32)
        tr[0, 0] = 0
        tr[1:, 0] = np.cumsum(em[:, self.blank])
        for t in range(T):
            tr[t + 1, 1:] = np.maximum(tr[t, 1:] + em[t, self.blank], tr[t, :-1] + em[t, tok])
        when, j = np.zeros(N, dtype=np.int64), N
        for t in range(T, 0, -1):
            if j == 0:
                break
            if tr[t - 1, j - 1] + em[t - 1, tok[j - 1]] >= tr[t - 1, j] + em[t - 1, self.blank]:
                when[j - 1] = t - 1
                j -= 1
        return when

    def align(self, piece_words, a, sr, t0):
        """Same contract as common.align: [char_start, char_end, t_start, t_end] per word."""
        dur = len(a) / sr
        n = len(piece_words)
        tokens, spans = [], []
        for _, _, w in piece_words:
            s = _DROP.sub("", w.upper().replace("’", "'"))
            ids = [self.vocab[c] for c in s if c in self.vocab]
            spans.append((len(tokens), len(tokens) + len(ids)) if ids else None)
            tokens += ids + [self.sep]
        em, fps = self._emissions(a, sr)
        if len(tokens) >= em.shape[0]:
            raise ValueError("audio is shorter than its text")
        when = self._path(em, tokens)
        start = [None if sp is None else when[sp[0]] / fps for sp in spans]
        end = [None if sp is None else min(dur, when[sp[1] - 1] / fps + 0.08) for sp in spans]
        # words without letters (numbers, dashes): spread evenly between their spoken neighbours
        i = 0
        while i < n:
            if start[i] is not None:
                i += 1
                continue
            j = i
            while j < n and start[j] is None:
                j += 1
            lo = end[i - 1] if i > 0 else 0.0
            hi = start[j] if j < n else dur
            step = max(0.0, hi - lo) / (j - i)
            for k in range(i, j):
                start[k], end[k] = lo + step * (k - i), lo + step * (k - i + 1)
            i = j
        out = []
        for k, (cs, ce, _) in enumerate(piece_words):
            s = max(0.0, min(dur, start[k]))
            e = max(s, min(dur, end[k]))
            if k + 1 < n:
                e = min(e, max(s, start[k + 1]))
            out.append([cs, ce, round(t0 + float(s), 3), round(t0 + float(e), 3)])
        return out
