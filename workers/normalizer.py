"""Optional hybrid normalization: deterministic rules first, an LLM only for the residue.

`make(url, model="", key=None, cache_dir=None, timeout=20.0, rules=True)` returns
`normalize(piece_words) -> list[str]`: element k is what to speak in place of word k of
the piece, so word counts and the character offsets of the original text are preserved
(timings, resume and highlighting are keyed on them).

The rule layer (spoken_rules.py, which needs the inflect package) labels every word
settled, default or flagged. Settled and default spoken forms are applied with no
network call, so with an empty url the rules run alone and flagged words are spoken as
written. When a url is set and the piece contains at least one default or flagged word,
the model is asked about exactly those words; a reply may change only them, and any
change to another word is dropped in code. Without inflect the rule layer is off (one
line on stderr) and the LLM is not asked either, so only the pronunciation table applies.

Only opening quotes/brackets stay in front of a replacement and only sentence
punctuation, closing quotes/brackets and dashes behind it, re-attached in code, and a
reply that is malformed, has out-of-range word numbers or changes an implausibly large
share of the words is discarded for that piece. Any network error or timeout returns
the words the rules produced, and after three consecutive failed requests the endpoint
is not called again in this process; this module never raises. Pieces that involved an
LLM call are cached under `cache_dir` (no caching when None), keyed by prompt version,
rules version, model name and piece text, so regenerating a chapter is free and
deterministic; rule-only pieces are never cached.
"""
import hashlib
import json
import os
import re
import sys
import urllib.error
import urllib.request

try:
    import spoken_rules
except ImportError:  # loaded from elsewhere (the eval harness); it lives in this folder
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import spoken_rules

PROMPT_VERSION = 12   # part of the cache key; bump whenever a prompt below changes
_LEGACY_VERSION = 5   # the pre-hybrid prompt, still used by the eval's LLM-only mode

# marked prompt: the hybrid asks only about default/flagged words; defaults arrive with
# their suggested spoken form, which the model confirms or corrects
_SYSTEM_M = (
    "You fix words that a speech engine would misread. The user message shows a sentence as a "
    "numbered list, where 1 is the first word. Marked words are the only ones you may change; "
    "every unmarked word is already settled. Most marked words carry a suggested spoken form "
    "in brackets, and the suggestion is usually right: change such a word only when the "
    "surrounding words clearly need another reading, for example a room number or a count "
    "where the suggestion reads a year. Words marked only with (?) have no suggestion; decide "
    "them yourself from context, and when you cannot tell the sense of such a word, leave it "
    "as written. Reply with ONLY one JSON object "
    '{"changes":[{"i":<word number>,"say":"<spoken form>"}]} listing the marked words that '
    "must be spoken differently; a marked word that is fine as written, or whose suggestion "
    "is right, is simply omitted. The say value is the spoken form in plain letters, no "
    "digits, no punctuation, at most eight words. Spoken form examples: past-tense read -> "
    "red, the metal lead -> led, live music -> lyve, the past of wind wound -> wowned, "
    "St. before a name -> Saint, St. after a name -> Street, Dr. -> Doctor or Drive, Jan. -> "
    "January."
)

# worked input/output pairs; small models follow these far better than instructions alone
_EXAMPLES_M = (
    "Marked words are the only ones you may change.\n"
    "Example 1:\n"
    "1: The\n2: drawer\n3: held\n4: two\n5: .38 (say: thirty eight)\n6: revolvers.\n"
    'Answer: {"changes":[]}\n'
    "Example 2:\n"
    "1: Yesterday\n2: she\n3: read (?)\n4: the\n5: note.\n"
    'Answer: {"changes":[{"i":3,"say":"red"}]}\n'
    "Example 3:\n"
    "1: She\n2: will\n3: lead (?)\n4: the\n5: team.\n"
    'Answer: {"changes":[]}\n'
    "Example 4:\n"
    "1: The\n2: clock\n3: wound (?)\n4: down\n5: before\n6: dawn.\n"
    'Answer: {"changes":[{"i":3,"say":"wowned"}]}\n'
    "Example 5:\n"
    "1: Meet\n2: me\n3: in\n4: room\n5: 240 (say: two hundred forty).\n"
    'Answer: {"changes":[{"i":5,"say":"two forty"}]}\n'
    "Example 6:\n"
    "1: Read (?)\n2: the\n3: sign\n4: before\n5: you\n6: enter.\n"
    'Answer: {"changes":[]}\n'
    "Example 7:\n"
    "1: They\n2: met\n3: on\n4: St. (say: Saint)\n5: James\n6: St. (say: Street)\n7: at\n8: noon.\n"
    'Answer: {"changes":[]}\n'
    "Example 8:\n"
    "1: The\n2: toy\n3: soldiers\n4: were\n5: cast\n6: from\n7: lead (?)\n8: back\n9: then.\n"
    'Answer: {"changes":[{"i":7,"say":"led"}]}\n'
    "Now:\n"
)

# the pre-hybrid prompt: the model picks the words itself; kept for the eval's LLM-only mode
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

def _mark(i, kinds, words):
    """Decoration for the numbered prompt: defaults show their suggested spoken form,
    flagged heteronyms the sense note from the table."""
    kind, say = kinds[i]
    if kind == spoken_rules.DEFAULT and say:
        return f" (say: {say})"
    if kind == spoken_rules.FLAGGED:
        w = spoken_rules._core(words[i]).lower()
        if w in spoken_rules.HETERONYMS:
            return f" (?) ({spoken_rules._HINTS[w]})" if w in spoken_rules._HINTS else " (?)"
        return " (?)"
    return ""


def _split(word, last=False, abbr=None):
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
    if abbr is None:  # no rule layer to ask: a short alphabetic token is taken for an abbreviation
        abbr = core.isalpha() and len(core) <= 4
    if abbr and not last and suffix.startswith("."):
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


def _cache_path(cache_dir, key):
    return os.path.join(cache_dir, hashlib.sha256(key.encode("utf-8")).hexdigest()[:32] + ".json")


def _stamp(path):
    """(mtime, size) of the pronunciations file, or None while it does not exist."""
    if not path:
        return None
    try:
        st = os.stat(path)
    except OSError:
        return None
    return (st.st_mtime_ns, st.st_size)


def make(url, model="", key=None, cache_dir=None, timeout=20.0, rules=True, pronounce=None):
    """Build the `normalize(piece_words) -> list[str]` function described in the docstring."""
    endpoint = url.rstrip("/") + "/chat/completions" if url else None
    headers = {"Content-Type": "application/json"}
    if key:
        headers["Authorization"] = "Bearer " + key
    table = spoken_rules.load_table(pronounce) if rules else {}  # the user's own pronunciations
    stamp = [_stamp(pronounce)]  # the state the table was loaded from; None = no file yet

    def refresh():
        """Reload the pronunciations when the file changed; no engine restart needed."""
        now = _stamp(pronounce)
        if now != stamp[0]:
            stamp[0] = now
            table.clear()
            table.update(spoken_rules.load_table(pronounce))

    level = [0]     # remembered request decoration: json_schema > json_object > plain
    failed = [0]    # consecutive failed requests; three open the breaker below
    broken = [False]
    warned = set()
    stats = {"pieces": 0, "residue": 0, "calls": 0, "rule_changed": 0, "changed": 0,
             "http_error": 0, "parse_fail": 0, "rejected": 0, "breaker": 0}

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
        out = list(words)
        kinds = [(spoken_rules.KEEP, None)] * len(words)
        if rules:
            refresh()  # an edited table applies to the next piece, whenever it arrives
            kinds = spoken_rules.apply(words, table)
            for i, (kind, say) in enumerate(kinds):
                if kind in (spoken_rules.SETTLED, spoken_rules.DEFAULT) and say:
                    nxt = words[i + 1] if i + 1 < len(words) else ""
                    lead, core, trail = _split(words[i], i == len(words) - 1, spoken_rules.is_abbrev(words[i], nxt))
                    if core:
                        out[i] = lead + say + trail
                        stats["rule_changed"] += 1
        if endpoint is None:
            return out
        marks = [i for i, (k, _) in enumerate(kinds)
                 if k in (spoken_rules.DEFAULT, spoken_rules.FLAGGED)]
        if rules and not marks:
            return out  # the rules settled everything; the endpoint is not needed
        stats["residue"] += 1
        ckey = f"{PROMPT_VERSION if rules else _LEGACY_VERSION}\n" \
               + (f"{spoken_rules.RULES_VERSION}\n{sorted(table.items())}\n" if rules else "") \
               + f"{model}\n{' '.join(words)}"
        if cache_dir is not None:
            try:
                with open(_cache_path(cache_dir, ckey), encoding="utf-8") as f:
                    cached = json.load(f)
                if isinstance(cached, list) and len(cached) == len(words) and all(isinstance(w, str) for w in cached):
                    return cached
            except (OSError, ValueError):
                pass
        if broken[0]:
            stats["breaker"] += 1
            return out
        if rules:
            marked = set(marks)
            user = _EXAMPLES_M + "\n".join(
                f"{i}: {w}{_mark(i - 1, kinds, words)}" for i, w in enumerate(words, 1)) + "\nAnswer:"
            messages = [{"role": "system", "content": _SYSTEM_M}, {"role": "user", "content": user}]
        else:
            user = _EXAMPLES + "\n".join(f"{i}: {w}" for i, w in enumerate(words, 1)) + "\nAnswer:"
            messages = [{"role": "system", "content": _SYSTEM}, {"role": "user", "content": user}]
        stats["calls"] += 1
        data = request(messages)
        if data is None:
            stats["http_error"] += 1
            failed[0] += 1
            trip()
            return out
        changes = _extract(_loads(_content(data)))
        if changes is None:
            stats["parse_fail"] += 1
            failed[0] += 1
            warn("parse", "a reply was not a valid change list; that piece is spoken as written")
            trip()
            return out
        failed[0] = 0  # the endpoint answered; a hanging server is not the problem
        if any(not 0 <= i - 1 < len(words) for i, _ in changes):
            stats["rejected"] += 1
            warn("reject", "a reply had an out-of-range word number; that piece is spoken as written")
            return out
        if len(changes) > max(2, len(words) // 4):  # densest legitimate case: two in nine words
            stats["rejected"] += 1
            warn("reject", "a reply changed too many words; that piece is spoken as written")
            return out
        for i, say in changes:
            if rules and i - 1 not in marked:  # settled and untouched words are not negotiable
                continue
            abbr = spoken_rules.is_abbrev(words[i - 1], words[i] if i < len(words) else "") if rules else None
            lead, core, trail = _split(words[i - 1], i == len(words), abbr)
            if rules and kinds[i - 1][0] == spoken_rules.FLAGGED and core.lower() in spoken_rules.HETERONYMS:
                want = spoken_rules.HETERONYMS[core.lower()]
                if " ".join(say.lower().replace("-", " ").split()) != " ".join(want.lower().replace("-", " ").split()):
                    continue  # a table heteronym is a yes/no call: the table spelling or nothing
            if not core or say.lower() == core.lower() or not _SAY.match(say) or not 1 <= len(say.split()) <= 8:
                continue  # not a spoken form we trust; keep the word the rules produced
            out[i - 1] = lead + say + trail  # kept punctuation around the replaced core
            stats["changed"] += 1
        if cache_dir is not None:
            try:
                os.makedirs(cache_dir, exist_ok=True)
                tmp = _cache_path(cache_dir, ckey) + ".part"
                with open(tmp, "w", encoding="utf-8") as f:
                    json.dump(out, f, ensure_ascii=False)
                os.replace(tmp, _cache_path(cache_dir, ckey))
            except OSError:
                pass  # a failing cache is not worth failing over
        return out

    normalize.stats = stats  # counters for tools/normalize_eval; not used by the workers
    return normalize
