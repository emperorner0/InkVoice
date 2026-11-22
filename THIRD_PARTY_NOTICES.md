# Third-party notices

InkVoice adapts material from the projects below. Each entry names the licence as
checked at the source in November 2026 and what was taken. Verify the licence at the
linked source before redistributing.

## Kokoro-FastAPI

Apache License 2.0. https://github.com/remsky/Kokoro-FastAPI, file
api/src/services/text_processing/normalization/english.py
(https://raw.githubusercontent.com/remsky/Kokoro-FastAPI/master/LICENSE).

The structure of the money pass in workers/spoken_rules.py (currency symbol followed
by a number body with thousands separators, a following magnitude word such as
million, cents for at most two decimals and a plain decimal reading beyond that, unit
words pluralised by count) and the idea of settling clock times, percentages and
dotted titles before any number pass are adapted from that file. Everything was
rewritten to decide one whitespace token at a time, so the file contains none of the
original lines.

## abogen

MIT License, Copyright (c) 2025 Deniz Şafak. https://github.com/denizsafak/abogen,
file abogen/kokoro_text_normalization.py.

The roman-numeral context rules in workers/spoken_rules.py (parse the numeral
subtractively, re-encode and compare so non-canonical forms are rejected; read a
cardinal after heading words such as chapter; read "the" plus an ordinal after a
monarch-style name or title) and entries of the title and rank abbreviation table
(Mr, Mrs, Ms, Prof, Rev, Gen, Sgt, Jr, Sr) are adapted from that file, again
rewritten for single-token decisions.

## inflect

MIT License. https://pypi.org/project/inflect/ (see its packaging metadata).

A runtime dependency of the rule layer rather than copied source. It supplies the
number wording (cardinals, ordinals, plural forms) through its public API.

## WikipediaHomographData

Apache License 2.0.
https://github.com/google-research-datasets/WikipediaHomographData.

The eval split of this dataset is downloaded by
tools/normalize_eval/homograph_eval.py into a git-ignored cache folder at eval time.
It is never committed to this repository and never shipped with the app.
