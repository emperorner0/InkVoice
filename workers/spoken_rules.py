"""Deterministic spoken-form rules: the first layer of hybrid normalization.

`apply(words) -> [(kind, say), ...]` gives every whitespace word of a piece one outcome:
"keep" (leave it alone), "settled" (say is final and the LLM may not touch the word),
"default" (say is used unless the LLM answers differently) or "flagged" (no safe
default; only the LLM can change it). normalizer.py attaches the say values with its
punctuation reattachment, so the one spoken token per original word contract holds by
construction. With no LLM configured, settled and default outcomes are applied and
flagged words are spoken as written.

Settled: money with a currency symbol (with or without a following magnitude word such
as "million"), percentages, clock times with a colon plus any a.m./p.m. word, suffix
ordinals, integers with thousands separators, and single-reading titles and month
abbreviations (Mr., Mrs., Ms., Prof., Sgt., Lt., Capt., Col., Gen., Mt., Rev., vs.,
Jr., Sr., Jan. and friends). Default: bare integers (year reading for 1100 to 2099
unless a plural noun follows), decimals, leading-dot caliber readings (.45s -> forty
fives, .357 -> three fifty seven, .30-06 -> thirty aught six), 9mm, digit-hyphen
compounds (12-gauge -> twelve gauge), Dr. and St. (Saint before a capitalised word,
Street after one), and roman numerals after Chapter/Book/Part/Act or a monarch's name.
Flagged: a fixed table of heteronyms with a usable respelling (past-tense read -> red,
the metal lead -> led, adjective live -> lyve, the past of wind wound -> wowned and
similar), abbreviation-looking tokens not in any table, and tokens containing digits
that no rule settled.

Number wording comes from inflect; without it the layer disables itself with one line
on stderr and apply() marks every word "keep" unless the pronunciation table lists it,
so normalizer.py does not ask the LLM either and only the table applies.

The money, clock time, percent and abbreviation passes are adapted from
Kokoro-FastAPI's Apache-2.0 api/src/services/text_processing/normalization/english.py
(https://github.com/remsky/Kokoro-FastAPI); the roman-numeral context rules and the
title/rank names from abogen's MIT abogen/kokoro_text_normalization.py
(https://github.com/denizsafak/abogen). Both were rewritten to decide one original
token at a time; see THIRD_PARTY_NOTICES.md.
"""
import os
import re
import sys
import unicodedata

RULES_VERSION = 3  # part of the normalizer cache key; bump whenever an outcome changes

KEEP, SETTLED, DEFAULT, FLAGGED = "keep", "settled", "default", "flagged"

try:
    import inflect
    _ENG = inflect.engine()
except Exception as _e:  # optional layer: never raise, tell the user once
    print(f"spoken_rules: inflect is unavailable ({_e}); the rule layer is off", file=sys.stderr)
    _ENG = None

_OPEN = "'\"“‘„¿¡([{<«"   # decoration dropped in front of a spoken form (same sets as
_CLOSE = "'\"”’.,;:!?…)]}>»—–-"  # normalizer._split, which reattaches what it keeps)

_TITLES = {  # single reading, dotted form; Dr. and St. are decided in code instead
    "mr": "Mister", "mrs": "Missus", "ms": "Miz", "prof": "Professor", "sgt": "Sergeant",
    "lt": "Lieutenant", "capt": "Captain", "col": "Colonel", "gen": "General", "mt": "Mount",
    "rev": "Reverend", "jr": "Junior", "sr": "Senior", "vs": "versus",
    "jan": "January", "feb": "February", "mar": "March", "apr": "April", "jun": "June",
    "jul": "July", "aug": "August", "sep": "September", "sept": "September", "oct": "October",
    "nov": "November", "dec": "December",
}
_DAYPART = {"a.m": "A M", "p.m": "P M"}              # matched with the internal dot kept
_MAGNITUDES = {"thousand", "million", "billion", "trillion",
               "thousands", "millions", "billions", "trillions"}
_UNITS = {"$": ("dollar", "cent"), "£": ("pound", "penny"), "€": ("euro", "cent")}
_HEADING = {"chapter", "book", "part", "act"}          # roman numeral reads as a cardinal
_REGAL_TITLE = {"king", "queen", "pope", "tsar", "czar", "emperor", "empress", "duke",
                "duchess", "prince", "princess", "kaiser", "shah", "sultan", "caliph",
                "archduke", "baron", "baroness", "earl"}
_REGAL_NAME = {"alfred", "alexander", "anne", "arthur", "albert", "catherine", "charles",
               "clement", "edward", "elizabeth", "frederick", "george", "gregory", "henry",
               "innocent", "ivan", "james", "john", "leopold", "louis", "mary", "napoleon",
               "nicholas", "otto", "paul", "peter", "philip", "phillip", "pius", "richard",
               "stephen", "urban", "victoria", "william"}
HETERONYMS = {  # flagged for the LLM; value is the respelling the non-default sense needs
    "read": "red", "lead": "led", "live": "lyve", "wound": "wowned", "bass": "base",
    "dove": "dohv", "minute": "my newt", "resume": "rezoomay", "refuse": "refyoos",
    "sake": "sahkee",
}
# Sense notes the LLM sees next to a flagged heteronym; only the words where the note
# measurably helps (bass, lead) carry one, the rest are asked plainly.
_HINTS = {
    "lead": "say led only for the metal; the verb to guide and uses like lead role stay as written",
    "bass": "say base only for music; the fish stays as written",
}
_NOT_PLURAL = {"is", "as", "us", "its", "his", "this", "thus", "was", "has", "does", "goes"}

_MONEY = re.compile(r"^([$£€])([\d,]*)(?:\.(\d+))?$")
_PERCENT = re.compile(r"^(\d{1,3}(?:,\d{3})+|\d+)(?:\.(\d+))?$")   # used behind a % sign
_CLOCK = re.compile(r"^(\d{1,2}):(\d{2})$")
_INT = re.compile(r"^\d+$")
_SAY_OK = re.compile(r"^[A-Za-z]+(?:[A-Za-z' -]*[A-Za-z])?$")     # a pronunciation table entry
_GROUPED = re.compile(r"^\d{1,3}(?:,\d{3})+$")
_DECIMAL = re.compile(r"^(\d+)\.(\d+)$")
_CALIBER = re.compile(r"^\.(\d{1,3})(?:-(\d{1,2}))?(s)?$")
_MM = re.compile(r"^(\d+)mm$", re.IGNORECASE)
_NUMWORD = re.compile(r"^(\d+)-([A-Za-z]+)$")
_ORDINAL = re.compile(r"^(\d+)(st|nd|rd|th)$")
_ROMAN_RE = re.compile(r"^[IVXLCDM]{2,}$")
_DOTTED_SHORT = re.compile(r"^(?:[A-Za-z]\.)+[A-Za-z]?$")         # U.S., e.g., a.m shapes
_SENT_END = re.compile(r"[.!?…][\"”’')\]]*$")  # a token that closed a sentence (for St.)
_RV = {"I": 1, "V": 5, "X": 10, "L": 50, "C": 100, "D": 500, "M": 1000}
_RO = [(1000, "M"), (900, "CM"), (500, "D"), (400, "CD"), (100, "C"), (90, "XC"),
       (50, "L"), (40, "XL"), (10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I")]
_DIGITS = ("zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine")


def _w(n):  # inflect cardinal as plain spaced words: no commas, no "and", no hyphens
    return _ENG.number_to_words(int(n), andword="").replace(",", "").replace("-", " ")


def _ord(n):
    return _ENG.ordinal(_w(n)).replace(",", "").replace("-", " ")


def _year(n):
    h, r = divmod(n, 100)
    if 2000 <= n <= 2009:  # "two thousand five", never "twenty oh five"
        return "two thousand" + ("" if r == 0 else " " + _w(r))
    if r == 0:
        return _w(h) + " hundred"
    return _w(h) + " " + ("oh " + _w(r) if r < 10 else _w(r))


def _decimal(ip, fp):
    return _w(int(ip or 0)) + " point" + "".join(" " + _DIGITS[int(c)] for c in fp)


def _plural_unit(unit, n):
    if unit == "penny":
        return "pence" if n != 1 else "penny"
    return unit + ("" if n == 1 else "s")


def _plural_word(s):
    head, _, last = s.rpartition(" ")
    if re.search(r"(?:s|x|z|ch|sh)$", last):
        last += "es"
    elif last.endswith("y") and last[-2:-1] not in "aeiou":
        last = last[:-1] + "ies"
    else:
        last += "s"
    return f"{head} {last}" if head else last


def _roman(s):  # abogen's validation trick: re-encode the parsed value and compare
    total, prev = 0, 0
    for ch in reversed(s):
        v = _RV[ch]
        if v < prev:
            total -= v
        else:
            total += v
            prev = v
    out, rest = "", total
    for a, b in _RO:
        while rest >= a:
            out += b
            rest -= a
    return total if out == s else None


def _core(word):
    """Token without decorative punctuation; $ £ € . - lead and % ° trail by meaning."""
    i, j = 0, len(word)
    while i < j and not word[i].isalnum() and word[i] not in "$£€.-":
        i += 1
    while j > i and not word[j - 1].isalnum() and word[j - 1] not in "%°":
        j -= 1
    return word[i:j]


def load_table(path):
    """User pronunciations from `path`: one `word = spoken form` per line, # starts a comment.
    -> {casefolded word: spoken form}. A missing file is an empty table; bad lines are skipped."""
    table = {}
    if not path or not os.path.isfile(path):
        return table
    try:
        with open(path, encoding="utf-8-sig") as f:
            for n, line in enumerate(f, 1):
                line = line.split("#", 1)[0].strip()
                if not line:
                    continue
                word, sep, say = (s.strip() for s in line.partition("="))
                if not sep or not word or " " in word or not _SAY_OK.match(say):
                    print(f"spoken_rules: pronunciations line {n} skipped (want: word = letters only)", file=sys.stderr)
                    continue
                table[_fold(word)] = say
    except OSError as e:
        print(f"spoken_rules: pronunciations not read: {e}", file=sys.stderr)
    return table


def _fold(word):
    return unicodedata.normalize("NFC", word).casefold().replace("’", "'")


def _listed(c, table):
    """Spoken form of a listed word, also for its plural and possessive."""
    low = _fold(c)
    if low in table:
        return table[low]
    for suffix in ("'s", "s"):
        if low.endswith(suffix) and low[:-len(suffix)] in table:
            return table[low[:-len(suffix)]] + suffix
    return None


def apply(words, table=None):
    """One (kind, say) per word; say is None unless the kind is settled or default. Words in the
    user's pronunciation table are settled first, whatever the rules would have said."""
    listed = [_listed(_core(w), table) if table else None for w in words]
    if _ENG is None:
        return [(SETTLED, s) if s else (KEEP, None) for s in listed]
    return [(SETTLED, s) if s else _safe(i, w, words) for i, (w, s) in enumerate(zip(words, listed))]


def _safe(i, w, words):
    """The rule outcome for one word; a word the rules fail on (inflect refuses numbers above
    its largest named scale) is spoken as written."""
    try:
        return _one(i, w, words)
    except Exception:
        return (KEEP, None)


def is_abbrev(word, nxt=""):
    """Whether a replaced token's trailing period is its own (Dr., Sgt., U.S., a.m.) and not the
    sentence's, so normalizer.py may drop it mid-piece."""
    c, after = _core(word), _core(nxt)
    low = c.lower()
    if low in _DAYPART or _DOTTED_SHORT.match(c):
        return not after[:1].isupper()  # "at 6 a.m. He left": that period also ends the sentence
    return low in _TITLES or low in ("dr", "st") or (c.isalpha() and len(c) <= 4 and after[:1].islower())


def _one(i, w, words):
    c = _core(w)
    if not c:
        return (KEEP, None)
    n = len(words)
    nxt = _core(words[i + 1]) if i + 1 < n else ""
    prv = _core(words[i - 1]) if i > 0 else ""
    low, cap = c.lower(), c[:1].isupper()
    if low in _MAGNITUDES and (pm := _MONEY.match(prv)):  # $2.4 million: the unit follows the scale
        return (SETTLED, f"{low} {_UNITS[pm.group(1)][0]}s")
    m = _MONEY.match(c)
    if m:
        unit, small = _UNITS[m.group(1)]
        ip, fp = m.group(2).replace(",", ""), m.group(3)
        if nxt.lower() in _MAGNITUDES:  # $2.4 million: the word carries the scale
            return (SETTLED, _decimal(ip, fp) if fp else _w(int(ip or 0)))
        if not fp:
            v = int(ip or 0)
            return (SETTLED, f"{_w(v)} {_plural_unit(unit, v)}")
        if len(fp) > 2:  # $3.499 a gallon: read the decimal, not 499 cents
            return (SETTLED, f"{_decimal(ip, fp)} {_plural_unit(unit, 2)}")
        v, k = int(ip or 0), int(fp.ljust(2, "0"))
        if v == 0:
            return (SETTLED, f"{_w(k)} {_plural_unit(small, k)}")
        return (SETTLED, f"{_w(v)} {_plural_unit(unit, v)} and {_w(k)} {_plural_unit(small, k)}")
    if c.endswith("%") and (m := _PERCENT.match(c[:-1])):
        v = _decimal(m.group(1).replace(",", ""), m.group(2)) if m.group(2) \
            else _w(int(m.group(1).replace(",", "")))
        return (SETTLED, v + " percent")
    if (m := _CLOCK.match(c)) and int(m.group(1)) <= 23 and int(m.group(2)) <= 59:
        h, mnt = int(m.group(1)), int(m.group(2))
        if mnt == 0:
            return (SETTLED, _w(h) + " o'clock")
        return (SETTLED, f"{_w(h)} " + ("oh " if mnt < 10 else "") + _w(mnt))
    if low in _DAYPART:
        return (SETTLED, _DAYPART[low])
    if (m := _CALIBER.match(c)):
        d = m.group(1)
        if d[0] == "0" or len(d) == 1:  # .05 is a decimal, and so is a bare .5
            say = "point" + "".join(" " + _DIGITS[int(ch)] for ch in d)
        elif len(d) == 2:
            say = _w(int(d))
        else:  # .308 -> three oh eight, .357 -> three fifty seven
            rest = int(d[1:])
            say = _w(int(d[0])) + " " + ("oh " if rest < 10 else "") + _w(rest)
        if m.group(2):
            say += " " + ("aught " + _w(int(m.group(2))) if m.group(2).startswith("0")
                          else _w(int(m.group(2))))
        return (DEFAULT, _plural_word(say) if m.group(3) else say)
    if (m := _DECIMAL.match(c)):
        return (DEFAULT, _decimal(m.group(1), m.group(2)))
    if (m := _MM.match(c)):
        return (DEFAULT, _w(int(m.group(1))) + " millimeter")
    if (m := _ORDINAL.match(c)):
        return (SETTLED, _ord(int(m.group(1))))
    if _GROUPED.match(c):
        return (SETTLED, _w(int(c.replace(",", ""))))
    if _INT.match(c):
        if nxt.lower() in _DAYPART:
            return (SETTLED, _w(int(c)))  # "6 a.m." -> "six A M"; aligners need letters
        v = int(c)
        if 1100 <= v <= 2099 and not (nxt.islower() and nxt.endswith("s")
                                      and nxt not in _NOT_PLURAL):
            return (DEFAULT, _year(v))
        return (DEFAULT, _w(v))
    if (m := _NUMWORD.match(c)):
        return (DEFAULT, f"{_w(int(m.group(1)))} {m.group(2)}")
    dotted = "." in w
    if dotted and (cap or low == "vs"):  # vs. stays lowercase; titles are capitalised
        if low == "st":
            if 1 < i and prv[:1].isupper() and not _SENT_END.search(words[i - 2]):
                return (DEFAULT, "Street")  # a name mid-sentence: Baker St., Alder St.
            return (DEFAULT, "Saint")       # otherwise the saint before a capitalised name
        if low == "dr":
            return (DEFAULT, "Doctor")
        if low in _TITLES:
            return (SETTLED, _TITLES[low])
    if _ROMAN_RE.match(c) or (c == "I" and prv.lower() in _HEADING | _REGAL_TITLE):
        v = _roman(c)
        if v and 1 <= v <= 100:
            if prv.lower() in _HEADING:
                return (DEFAULT, _w(v))
            if prv[:1].isupper() and prv.lower() in _REGAL_NAME | _REGAL_TITLE:
                return (DEFAULT, "the " + _ord(v))
            if len(c) > 1:
                return (FLAGGED, None)  # a roman numeral with no context, e.g. World War II
    if low in HETERONYMS:
        return (FLAGGED, None)
    if dotted and c not in ("I", "A") and (
            _DOTTED_SHORT.match(c) or (cap and len(c) <= 3 and nxt[:1].islower())):
        return (FLAGGED, None)  # U.S., e.g., or Jan. before a lowercase word; never "Tom."
    if any(ch.isdigit() for ch in c):
        return (FLAGGED, None)
    return (KEEP, None)
