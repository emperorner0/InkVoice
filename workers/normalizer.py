"""Optional LLM normalization pass: supply spoken forms for words a TTS engine misreads.

`make(url, model, key=None, cache_dir=None, timeout=20.0)` returns `normalize(piece_words) ->
list[str]`: element k is what to speak in place of word k of the piece, so word counts and the
character offsets of the original text are preserved (timings, resume and highlighting are keyed
on them).  The model only picks words to replace; only opening quotes/brackets stay in front of a
replacement and only sentence punctuation, closing quotes/brackets and dashes behind it, re-attached
in code, and a reply that is malformed, has out-of-range word numbers or changes an implausibly
large share of the words is discarded for that piece.  Any network error or timeout returns the
original words, and after three consecutive failed requests the endpoint is not called again in
this process; this module never raises and needs only the stdlib.  Successful pieces are cached
under `cache_dir` (no caching when None), keyed by prompt version, model name and piece text, so
regenerating a chapter is free and deterministic.
"""
import hashlib
import json
import os
import re
import sys
import urllib.error
import urllib.request

PROMPT_VERSION = 5  # part of the cache key; bump whenever the prompt below changes
_SYSTEM = (
    "You fix words that a speech engine would misread. The user message shows worked examples "
    "and then a sentence as a numbered list, where 1 is the first word. Reply with ONLY one JSON "
    'object {"changes":[{"i":<word number>,"say":"<spoken form>"}]} listing every word that must '
    "be changed and nothing else. Change only words whose written form is misread aloud: digits "
    "and numbers, money, clock times, roman numerals, military and academic titles, and ambiguous "
    "words such as read or lead. The say value is the spoken form in plain letters, no digits, no "
    "punctuation. Spoken form examples: 1776 -> seventeen seventy six, 1984 -> nineteen eighty "
    "four, .45s -> forty fives, .357 -> three fifty seven, .30-06 -> thirty aught six, $9.50 -> "
    "nine dollars fifty, $19.99 -> nineteen dollars ninety nine, $500 -> five hundred dollars, 10% "
    "-> ten percent, 3:30 -> three thirty, IV in a chapter heading -> four, VIII after a king -> "
    "the eighth, II after a queen -> the second, Dr. -> Doctor, Sgt. -> Sergeant, Lt. -> "
    "Lieutenant, Prof. -> Professor, vs. -> "
    "versus, St. -> Saint, Mt. -> Mount, 6 a.m. -> six a em, past-tense read -> red, the metal "
    "lead -> led, live "
    "music -> lyve, a wound up clock -> wowned. The word CHAPTER itself "
    "never changes, only the numeral after it; lead meaning to guide (she will lead) is left "
    "alone, only the metal becomes led. Ordinary words are never changed."
)

# worked input/output pairs; small models follow these far better than instructions alone
_EXAMPLES = (
    "Example 1:\n"
    "1: Lunch\n2: came\n3: to\n4: $9.50\n5: with\n6: coffee.\n"
    'Answer: {"changes":[{"i":4,"say":"nine dollars fifty"}]}\n'
    "Example 2:\n"
    "1: Yesterday\n2: he\n3: read\n4: the\n5: note.\n"
    'Answer: {"changes":[{"i":3,"say":"red"}]}\n'
    "Example 3:\n"
    "1: The\n2: kettle\n3: whistled\n4: while\n5: the\n6: bread\n7: toasted.\n"
    'Answer: {"changes":[]}\n'
    "Example 4:\n"
    "1: The\n2: letter\n3: opened\n4: with\n5: CHAPTER\n6: IV\n7: in\n8: print.\n"
    'Answer: {"changes":[{"i":6,"say":"four"}]}\n'
    "Example 5:\n"
    "1: She\n2: will\n3: lead\n4: the\n5: team.\n"
    'Answer: {"changes":[]}\n'
    "Now:\n"
)

_SCHEMA = {
    "type": "object",
    "properties": {"changes": {"type": "array", "items": {
        "type": "object",
        "properties": {"i": {"type": "integer"}, "say": {"type": "string"}},
        "required": ["i", "say"],
        "additionalProperties": False,
    }}},
    "required": ["changes"],
    "additionalProperties": False,
}
_SAY = re.compile(r"^[A-Za-z]+(?:[A-Za-z' -]*[A-Za-z])?$")  # letters, spaces, hyphens, apostrophes
_OPEN = "'\"“‘„¿¡([{<«"   # kept in front of a replaced core
_CLOSE = "'\"”’.,;:!?…)]}>»—–-"  # kept behind it: sentence punctuation, closers, dashes


def _split(word, last=False):
    """Split a token into (prefix kept, core replaced, suffix kept) for a replacement.

    Only opening quotes/brackets are kept in front; behind, only sentence punctuation, closing
    quotes/brackets and dashes.  Symbols that are part of the spoken thing ($, %, °, a leading
    decimal point) and an abbreviation's own period (dropped unless the word ends the piece) go
    with the core.
    """
    i, j = 0, len(word)
    while i < j and not word[i].isalnum():
        i += 1
    while j > i and not word[j - 1].isalnum():
        j -= 1
    lead, core, trail = word[:i], word[i:j], word[j:]
    k = len(lead)
    while k > 0 and lead[k - 1] in _OPEN:
        k -= 1
    m = len(trail)
    while m > 0 and trail[m - 1] in _CLOSE:
        m -= 1
    suffix = trail[m:]
    if core.isalpha() and not last and suffix.startswith("."):
        suffix = suffix[1:]  # 'Dr.' mid-sentence: that period belongs to the abbreviation
    return lead[k:], core, suffix


def _loads(text):
    if not isinstance(text, str):
        return None
    text = text.strip()
    if text.startswith("```"):  # some models fence the JSON anyway
        lines = text.splitlines()
        text = "\n".join(lines[1:-1] if len(lines) > 1 and lines[-1].startswith("```") else lines[1:])
    try:
        return json.loads(text)
    except ValueError:
        pass
    for a, b in (("{", "}"), ("[", "]")):  # JSON wrapped in prose
        i, j = text.find(a), text.rfind(b)
        if 0 <= i < j:
            try:
                return json.loads(text[i:j + 1])
            except ValueError:
                pass
    return None


def _extract(obj):
    """[(word number, spoken form)] from a reply, or None when the shape is wrong."""
    if isinstance(obj, dict):
        obj = obj.get("changes")
    if not isinstance(obj, list):
        return None
    out = []
    for ch in obj:
        if not isinstance(ch, dict):
            return None
        i, say = ch.get("i", ch.get("index")), ch.get("say")
        if isinstance(i, str) and i.isdigit():
            i = int(i)
        if isinstance(i, bool) or not isinstance(i, int) or not isinstance(say, str):
            return None
        out.append((i, say.strip()))
    return out


def _content(data):
    try:
        c = data["choices"][0]["message"]["content"]
    except (KeyError, IndexError, TypeError):
        return None
    return c if isinstance(c, str) else None


def _cache_path(cache_dir, model, words):
    h = hashlib.sha256(f"{PROMPT_VERSION}\n{model}\n{' '.join(words)}".encode("utf-8")).hexdigest()
    return os.path.join(cache_dir, h[:32] + ".json")


def make(url, model, key=None, cache_dir=None, timeout=20.0):
    """Build the `normalize(piece_words) -> list[str]` function described in the docstring."""
    endpoint = url.rstrip("/") + "/chat/completions"
    headers = {"Content-Type": "application/json"}
    if key:
        headers["Authorization"] = "Bearer " + key
    level = [0]     # remembered request decoration: json_schema > json_object > plain
    failed = [0]    # consecutive failed requests; three open the breaker below
    broken = [False]
    warned = set()
    stats = {"pieces": 0, "changed": 0, "http_error": 0, "parse_fail": 0, "rejected": 0, "breaker": 0}

    def warn(kind, msg):
        if kind not in warned:  # at most one stderr line per kind of failure
            warned.add(kind)
            print(f"normalizer: {msg}", file=sys.stderr)

    def trip():
        if failed[0] >= 3 and not broken[0]:
            broken[0] = True
            warn("breaker", "three consecutive failed requests; normalization is off for this run")

    def request(messages):
        base = {"model": model, "messages": messages, "temperature": 0, "max_tokens": 512}
        variants = [  # constrained output and thinking off when the endpoint allows it
            dict(base, chat_template_kwargs={"enable_thinking": False},
                 response_format={"type": "json_schema", "json_schema": {"name": "changes", "schema": _SCHEMA}}),
            dict(base, chat_template_kwargs={"enable_thinking": False},
                 response_format={"type": "json_object"}),
            base,
        ]
        for lv in range(level[0], len(variants)):
            req = urllib.request.Request(endpoint, data=json.dumps(variants[lv]).encode(),
                                         headers=headers, method="POST")
            try:
                with urllib.request.urlopen(req, timeout=timeout) as r:
                    data = json.loads(r.read().decode("utf-8", "replace"))
                level[0] = lv
                return data
            except urllib.error.HTTPError as e:
                if e.code in (400, 404, 422) and lv < len(variants) - 1:
                    try:
                        e.read()
                    except Exception:
                        pass
                    continue  # the endpoint dislikes an optional field; retry plainer
                warn("http", f"request failed: {e}")
                return None
            except Exception as e:
                warn("http", f"request failed: {e}")
                return None
        return None

    def normalize(piece_words):
        words = [w for _, _, w in piece_words]
        stats["pieces"] += 1
        if cache_dir is not None:
            try:
                with open(_cache_path(cache_dir, model, words), encoding="utf-8") as f:
                    cached = json.load(f)
                if isinstance(cached, list) and len(cached) == len(words) and all(isinstance(w, str) for w in cached):
                    return cached
            except (OSError, ValueError):
                pass
        if broken[0]:
            stats["breaker"] += 1
            return words
        user = _EXAMPLES + "\n".join(f"{i}: {w}" for i, w in enumerate(words, 1)) + "\nAnswer:"
        data = request([{"role": "system", "content": _SYSTEM}, {"role": "user", "content": user}])
        if data is None:
            stats["http_error"] += 1
            failed[0] += 1
            trip()
            return words
        changes = _extract(_loads(_content(data)))
        if changes is None:
            stats["parse_fail"] += 1
            failed[0] += 1
            warn("parse", "a reply was not a valid change list; that piece is spoken as written")
            trip()
            return words
        failed[0] = 0  # the endpoint answered; a hanging server is not the problem
        if any(not 0 <= i - 1 < len(words) for i, _ in changes):
            stats["rejected"] += 1
            warn("reject", "a reply had an out-of-range word number; that piece is spoken as written")
            return words
        if len(changes) > max(2, len(words) // 4):  # densest legitimate case: two in nine words
            stats["rejected"] += 1
            warn("reject", "a reply changed too many words; that piece is spoken as written")
            return words
        out = list(words)
        for i, say in changes:
            lead, core, trail = _split(words[i - 1], i == len(words))
            if not core or say.lower() == core.lower() or not _SAY.match(say) or not 1 <= len(say.split()) <= 8:
                continue  # not a spoken form we trust; keep the original word
            out[i - 1] = lead + say + trail  # kept punctuation around the replaced core
            stats["changed"] += 1
        if cache_dir is not None:
            try:
                os.makedirs(cache_dir, exist_ok=True)
                tmp = _cache_path(cache_dir, model, words) + ".part"
                with open(tmp, "w", encoding="utf-8") as f:
                    json.dump(out, f, ensure_ascii=False)
                os.replace(tmp, _cache_path(cache_dir, model, words))
            except OSError:
                pass  # a failing cache is not worth failing over
        return out

    normalize.stats = stats  # counters for tools/normalize_eval; not used by the workers
    return normalize
