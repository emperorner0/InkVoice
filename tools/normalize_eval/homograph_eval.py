"""Score the hybrid normalizer on the WikipediaHomographData eval split.

Usage: python tools/normalize_eval/homograph_eval.py --url http://127.0.0.1:8091/v1 --model <name>

The sentences of the eval split for the heteronyms in workers/spoken_rules.py's
HETERONYMS table are downloaded from
https://github.com/google-research-datasets/WikipediaHomographData (Apache-2.0) into
homograph-cache/ next to this script (git-ignored; delete it to re-download). Each
sentence runs through the hybrid normalizer as one piece and the target word, located
by the dataset's character offsets, must come back respelled when its sense needs the
respelling and unchanged otherwise. Reports accuracy per word and overall.
"""
import argparse
import csv
import os
import re
import sys
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CACHE = Path(__file__).resolve().parent / "homograph-cache"
BASE = "https://raw.githubusercontent.com/google-research-datasets/WikipediaHomographData/master/data"
WORD = re.compile(r"\S+")

# wordid -> the spoken form that sense needs; None means spoken as written. The wordids
# and their senses come from the dataset's data/wordids.tsv.
SENSES = {
    "read": {"read_past": "red", "read_present": None},
    "lead": {"lead_nou": "led", "lead_nou-vrb": None},
    "live": {"live_adj": "lyve", "live_vrb": None},
    "wound": {"wound_vrb": "wowned", "wound_nou-vrb": None},
    "bass": {"bass": "base", "bass_corp": None},
    "dove": {"dove_vrb": "dohv", "dove": None},
    "minute": {"minute_adj": "my newt", "minute": None},
    "resume": {"resume_nou": "rezoomay", "resume_vrb": None},
    "refuse": {"refuse_nou": "refyoos", "refuse_vrb": None},
    "sake": {"sake_jp": "sahkee", "sake": None},
}


def fetch(word):
    path = CACHE / f"{word}.tsv"
    if not path.exists():
        os.makedirs(CACHE, exist_ok=True)
        tmp = path.with_suffix(".part")
        urllib.request.urlretrieve(f"{BASE}/eval/{word}.tsv", tmp)
        os.replace(tmp, path)
    rows = []
    with open(path, encoding="utf-8", newline="") as f:
        for homograph, wordid, sentence, start, end in csv.reader(f, delimiter="\t"):
            if homograph and homograph != "homograph":
                rows.append((wordid, sentence, int(start), int(end)))
    return rows


def form(s):  # comparison form: case-insensitive, hyphens as spaces, collapsed whitespace
    return " ".join(s.lower().replace("-", " ").split())


def core(word):  # word without leading/trailing punctuation (same rule as normalizer._split)
    i, j = 0, len(word)
    while i < j and not word[i].isalnum():
        i += 1
    while j > i and not word[j - 1].isalnum():
        j -= 1
    return word[i:j]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", required=True, help="OpenAI-compatible endpoint ending in /v1")
    ap.add_argument("--model", required=True)
    ap.add_argument("--timeout", type=float, default=20.0)
    args = ap.parse_args()

    sys.path.insert(0, str(ROOT / "workers"))
    import normalizer
    normalize = normalizer.make(args.url, args.model, key=os.environ.get("INKVOICE_NORMALIZE_KEY"),
                                timeout=args.timeout)
    stats = normalize.stats

    per, misses = {}, []
    total = {"cases": 0, "correct": 0}
    for word in sorted(SENSES):
        d = per.setdefault(word, {"cases": 0, "correct": 0})
        for wordid, sentence, start, end in fetch(word):
            want = SENSES[word].get(wordid)
            if want is None and wordid not in SENSES[word]:
                continue  # a wordid this table does not know
            piece = [(m.start(), m.end(), m.group()) for m in WORD.finditer(sentence)]
            idx = [i for i, (s, e, _) in enumerate(piece) if s <= start < e]
            if not idx:
                print(f"offsets {start}:{end} match no word: {sentence[:60]}", file=sys.stderr)
                continue
            i = idx[0]
            t0 = time.perf_counter()
            out = normalize(piece)
            del t0
            got, was = out[i], piece[i][2]
            # an unchanged sense only requires the target word itself to survive; other
            # parts of its token may still be normalized ("30-minute" -> "thirty minute")
            ok = form(core(got)) == form(want) if want else form(sentence[start:end]) \
                in form(core(got)).split()
            d["cases"] += 1
            total["cases"] += 1
            for acc in (d, total):
                if ok:
                    acc["correct"] += 1
            if not ok:
                misses.append(f"{word}/{wordid}: {was} -> {got} (want {want or 'unchanged'}) "
                              f"in: {sentence[:70]}")

    print(f"model: {args.model}  sentences: {total['cases']}  "
          f"LLM pieces: {stats['residue']} of {stats['pieces']}")
    hdr = f"{'word':<10}{'cases':>7}{'ok':>5}{'share':>8}"
    print(hdr)
    print("-" * len(hdr))
    for word in sorted(per):
        d = per[word]
        share = d["correct"] / d["cases"] if d["cases"] else 1.0
        print(f"{word:<10}{d['cases']:>7}{d['correct']:>5}{share:>8.0%}")
    print("-" * len(hdr))
    share = total["correct"] / total["cases"] if total["cases"] else 0.0
    print(f"{'overall':<10}{total['cases']:>7}{total['correct']:>5}{share:>8.0%}")
    if misses:
        print(f"\nmisses ({len(misses)}):")
        for m in misses:
            print("  " + m)


if __name__ == "__main__":
    sys.exit(main())
