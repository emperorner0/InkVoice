"""Measure workers/normalizer.py against cases.jsonl on a running OpenAI-compatible server.

Usage: python tools/normalize_eval/run_eval.py --url http://127.0.0.1:8091/v1 --model <name>
The optional API key comes from INKVOICE_NORMALIZE_KEY, as for the workers. The disk cache is
disabled here so every case exercises the server. Each case is one piece; per category this
reports the share of expected changes made correctly, false changes on words that must stay
unchanged, invalid or discarded replies, and latency p50/p95 per piece. Spoken forms are compared
case-insensitively with hyphens treated as spaces.
"""
import argparse
import importlib.util
import json
import os
import re
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WORD = re.compile(r"\S+")


def load_normalizer():
    spec = importlib.util.spec_from_file_location("normalizer", ROOT / "workers" / "normalizer.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def form(s):  # comparison form: case-insensitive, hyphens as spaces, collapsed whitespace
    return " ".join(s.lower().replace("-", " ").split())


def core(word):  # word without leading/trailing punctuation (same rule as normalizer._split)
    i, j = 0, len(word)
    while i < j and not word[i].isalnum():
        i += 1
    while j > i and not word[j - 1].isalnum():
        j -= 1
    return word[i:j]


def pctile(xs, p):
    if not xs:
        return 0.0
    xs = sorted(xs)
    return xs[min(len(xs) - 1, round(p / 100 * (len(xs) - 1)))]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", required=True, help="OpenAI-compatible endpoint ending in /v1")
    ap.add_argument("--model", required=True)
    ap.add_argument("--cases", default=str(Path(__file__).resolve().parent / "cases.jsonl"))
    ap.add_argument("--timeout", type=float, default=20.0)
    args = ap.parse_args()

    normalizer = load_normalizer()
    normalize = normalizer.make(args.url, args.model, key=os.environ.get("INKVOICE_NORMALIZE_KEY"),
                                timeout=args.timeout)
    stats = normalize.stats

    cases = [json.loads(l) for l in open(args.cases, encoding="utf-8") if l.strip()]
    per, overall = {}, {"expected": 0, "correct": 0, "false": 0, "words": 0, "discarded": 0,
                        "lat": [], "wrong": []}
    for case in cases:
        cat = case["category"]
        d = per.setdefault(cat, {"cases": 0, "expected": 0, "correct": 0, "false": 0,
                                 "words": 0, "discarded": 0, "lat": [], "wrong": []})
        words = WORD.findall(case["text"])
        piece = [(m.start(), m.end(), m.group()) for m in WORD.finditer(case["text"])]
        want = {int(k): [form(f) for f in v] for k, v in case["changes"].items()}
        before = (stats["parse_fail"], stats["rejected"], stats["http_error"])
        t0 = time.perf_counter()
        out = normalize(piece)
        dt = (time.perf_counter() - t0) * 1000
        discarded = any(b - a for a, b in zip(before, (stats["parse_fail"], stats["rejected"], stats["http_error"], stats["breaker"])))

        false = 0
        for k, (got, was) in enumerate(zip(out, words)):
            if k in want:
                if form(core(got)) in want[k]:
                    for acc in (d, overall):
                        acc["correct"] += 1
                else:
                    d["wrong"].append(f'{case["text"][:40]}: {was} -> {got}')
            elif got != was and form(core(got)) != form(core(was)):
                false += 1
                d["wrong"].append(f'{case["text"][:40]}: {was} -> {got}')
        d["cases"] += 1
        for acc in (d, overall):
            acc["expected"] += len(want)
            acc["false"] += false
            acc["words"] += len(words)
            acc["discarded"] += int(discarded)
            acc["lat"].append(dt)

    print(f"model: {args.model}  cases: {len(cases)}  words: {overall['words']}")
    hdr = f"{'category':<14}{'cases':>6}{'exp':>5}{'ok':>5}{'share':>8}{'false':>7}{'disc':>6}{'p50ms':>8}{'p95ms':>8}"
    print(hdr)
    print("-" * len(hdr))
    for cat in sorted(per):
        d = per[cat]
        share = d["correct"] / d["expected"] if d["expected"] else 1.0
        print(f"{cat:<14}{d['cases']:>6}{d['expected']:>5}{d['correct']:>5}{share:>8.0%}"
              f"{d['false']:>7}{d['discarded']:>6}{pctile(d['lat'], 50):>8.0f}{pctile(d['lat'], 95):>8.0f}")
    print("-" * len(hdr))
    share = overall["correct"] / overall["expected"]
    print(f"{'overall':<14}{len(cases):>6}{overall['expected']:>5}{overall['correct']:>5}{share:>8.0%}"
          f"{overall['false']:>7}{overall['discarded']:>6}"
          f"{pctile(overall['lat'], 50):>8.0f}{pctile(overall['lat'], 95):>8.0f}")
    wrong = [w for d in per.values() for w in d["wrong"]]
    if wrong:
        print(f"\nmisses and false changes ({len(wrong)}):")
        for w in wrong:
            print("  " + w)


if __name__ == "__main__":
    sys.exit(main())
