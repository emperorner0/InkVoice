# Text normalization

An optional pass that fixes the words a synthesis engine misreads before they are spoken:
firearm calibers (`.45s`, `5.56`), money (`$9.50`), years (`1984`), clock times (`3:30`),
roman numerals in chapter headings and after monarchs, titles (`Dr.`, `Sgt.`, `vs.`), and
words such as a past-tense `read` or the metal `lead`.

The pass has two layers. A deterministic rule layer runs first on every piece, without any
network access, and settles most of these words on its own. A language model is asked only
about the words the rules cannot settle, so it sees a handful of marked words instead of the
whole sentence. Everything runs offline by default: with no server configured, the rules run
alone.

## The two layers

The rule layer (`workers/spoken_rules.py`, number wording by the `inflect` package) gives
every word of a piece one of four outcomes:

- settled: the rule's spoken form is final. Money with a currency symbol (including a
  following magnitude word such as "million"), percentages, clock times with a colon plus
  any a.m./p.m. word, suffix ordinals (`21st`), integers with thousands separators, and
  single-reading titles and month abbreviations (Mr., Mrs., Prof., Sgt., Lt., vs., Jan.).
- default: the rule proposes a spoken form that is used unless the model, when one is
  configured, answers differently. Bare integers (a four-digit number from 1100 to 2099
  reads as a year unless a plural noun follows), decimals, leading-dot calibers (`.45s` ->
  "forty fives", `.357` -> "three fifty seven", `.30-06` -> "thirty aught six"), `9mm`,
  digit compounds such as `12-gauge`, Dr. and St. (Saint before a capitalised word, Street
  after one), and roman numerals after Chapter/Book/Part/Act or a monarch's name.
- flagged: no safe default exists, so only the model can change the word. A fixed table of
  heteronyms with a usable respelling (past-tense read -> "red", the metal lead -> "led",
  adjective live -> "lyve", the past of wind wound -> "wowned", music bass -> "base" and a
  few more), abbreviation-looking tokens the tables do not know (Jan. before a lowercase
  word, U.S., e.g.), and any leftover token containing digits (`350°`).
- unchanged: everything else.

The model is called only when a server address is configured and the piece contains at
least one default or flagged word. The prompt shows the piece as a numbered word list with
the uncertain words marked: defaults carry their suggested spoken form, which the model
confirms or corrects, and flagged words carry a question mark (heteronyms from the table
also show their sense note). A reply may change only the marked words; a change to any
other word is dropped in code. For a table heteronym the model is further restricted to the
table's respelling or no change, so it acts as a sense check and cannot invent a spelling.

With no server configured the rules still run: settled and default words are spoken in
their rule form and flagged words are spoken as written. When `inflect` cannot be imported,
the rule layer turns itself off with one line on the worker's stderr and the model is not
asked either, even when a server is configured, so only your pronunciation table applies.
Nothing raises.

## Your own pronunciations

Some words are read differently from one occurrence to the next, typically names and loanwords
with accented letters. To pin one down, add it to the pronunciations table: right-click the
word while reading and choose "Set pronunciation…", or use the Pronunciations list in Settings
(inside "Pronunciation helper (experimental)", which has to be enabled). Both write
`<InkVoice data>/pronunciations.txt`, one `word = spoken form` line per entry:

```
# lines starting with # are comments
doppelgänger = doppelganger
Nguyen = nwin
```

The file stays hand-editable, including while the app is running. A listed word is always
spoken in the given form, before any other rule and without asking the model. Matching ignores
case and also covers the plural and the possessive of the word. The spoken form may contain
only letters, spaces, hyphens and apostrophes; anything else is rejected with a short note.
A change applies to audio generated afterwards, without restarting the engine or clearing the
generation queue. Chapters that already have audio keep the old reading until they are
regenerated, and after a change the app offers to regenerate exactly the ones that contain
the word.

## Turning it on

In Settings, open "Pronunciation helper (experimental)" below the voice settings and tick
Enabled. The rule layer then runs without any server. To add the model pass, fill in the
server address (hint: `http://127.0.0.1:8091/v1`), the model name, and an optional API key.
The key is passed to the worker process in the `INKVOICE_NORMALIZE_KEY` environment
variable and sent as a bearer token, so it never appears on a command line. Any server that
implements `POST /v1/chat/completions` works: llama.cpp's `llama-server`, LM Studio, vLLM,
or a hosted provider.

For a local setup, serve a small instruct model with llama.cpp, for example:

```
llama-server -m <model>.gguf -ngl 99 -c 4096 --host 127.0.0.1 --port 8091 --jinja
```

On the worker command line, `--normalize-rules` turns on the rule layer alone and
`--normalize-url` (with `--normalize-model`) adds the model pass. Without either flag the
workers behave exactly as before.

## The word-for-word guarantee

Word timings, resume and highlighting are keyed on character offsets of the original text,
so the pass can never add, drop or reorder words. For every piece it returns exactly one
spoken token per original word, and most words come back unchanged.

A replacement keeps only the original word's opening quotes or brackets in front, and only
sentence punctuation, closing quotes or brackets and dashes behind. A leading decimal
point, `$`, `%` or `°` is part of the spoken thing and goes with the replacement, and an
abbreviation's own period is dropped (`Dr.` mid-sentence becomes `Doctor`, with the
sentence's final period kept when the word ends the piece). Every guarantee is enforced in
code in `workers/normalizer.py`; neither the rules nor the model pick the punctuation. A
spoken form may contain only letters, spaces, hyphens and apostrophes, at most eight words,
no digits.

## Fallbacks

When anything goes wrong, the pass returns the words the rules produced (or the original
words when the rules are off):

- A reply that is not valid JSON, uses an out-of-range word number, or changes more than a
  quarter of a piece's words (two words minimum) is discarded for that piece.
- Network errors and timeouts return the rule output. After three consecutive failed
  requests the endpoint is not called again for the rest of the worker's life, so a hung
  server costs at most three timeouts per chapter instead of one per piece.
- Each kind of failure prints at most one line on the worker's stderr.

The request asks for schema-constrained JSON output and disables model thinking where
supported; servers that reject those fields are retried without them.

## Cache

Pieces that involved a model call are cached in `<InkVoice data>/normalize-cache/`, one
small JSON file per piece, keyed by prompt version, rules version, model name and the piece
text. Regenerating a chapter, or streaming it a second time, makes no calls to the endpoint
at all. Pieces the rules settled on their own are never cached, since they are free to
begin with. Delete the folder to redo everything; the cache writes are atomic and a failed
write is ignored.

## Running the evaluation

`tools/normalize_eval/cases.jsonl` holds 93 original sentences with the expected spoken
forms, grouped by category (calibers, decimals, money, dates, clock times, ordinals,
abbreviations, respellable heteronyms, and controls that must not change). The harness has
three modes; `rules` needs no server:

```
python tools/normalize_eval/run_eval.py --mode rules
python tools/normalize_eval/run_eval.py --mode hybrid --url http://127.0.0.1:8091/v1 --model <name>
python tools/normalize_eval/run_eval.py --mode llm --url http://127.0.0.1:8091/v1 --model <name>
```

The eval disables the cache and reports, per category, the share of expected changes made
correctly, false changes on words that should not change, invalid or discarded replies,
the share of pieces that needed a model call, and latency p50/p95 per piece. Spoken forms
are compared case-insensitively with hyphens treated as spaces.

The heteronym layer is scored against the WikipediaHomographData eval split
(Apache-2.0, downloaded into a git-ignored cache folder on first use, never committed):

```
python tools/normalize_eval/homograph_eval.py --url http://127.0.0.1:8091/v1 --model <name>
```

It checks that the target word comes back respelled when its sense needs the respelling
and unchanged otherwise, and reports accuracy per word and overall.

## Results

Measured on one RTX 4070 SUPER (12 GB) with llama.cpp, context 4096, all layers on the
GPU, rules revision 3 and prompt revision 12 (2026-11). 93 sentences, 824 words, 85
expected changes. Qwen3-4B-Instruct-2507 Q4_K_M as the model where one is used. The rules
were tuned on this same case set, so the scores confirm that the listed cases are covered
and say little about unseen text.

Share of expected changes made correctly, per category:

| Category | Rules only | LLM only | Hybrid |
|---|---|---|---|
| Abbreviations and ranks | 100% | 86% | 100% |
| Calibers and gauges | 100% | 80% | 100% |
| Dates and years | 100% | 92% | 100% |
| Decimals and measurements | 92% | 92% | 100% |
| Heteronyms (read, lead, live, wound) | 0% | 100% | 100% |
| Money | 100% | 54% | 100% |
| Ordinals and roman numerals | 89% | 89% | 100% |
| Clock times | 100% | 71% | 100% |

| | Rules only | LLM only | Hybrid |
|---|---|---|---|
| Expected changes correct | 75 of 85 (88%) | 70 of 85 (82%) | 85 of 85 (100%) |
| False changes | 0 | 5 | 0 |
| Invalid or discarded replies | 0 | 0 | 0 |
| Pieces needing a model call | 0 | 100% | 54% |
| Latency per piece, p50 / p95 | 0 ms | 123 / 190 ms | 59 / 143 ms |

The rules alone already cover everything deterministic; their misses are exactly the
flagged words (heteronyms, `350°`, a roman numeral without context). The model alone merges
adjacent words (`6 a.m.` into one change, `(up 12%)` shifted onto the wrong word) and reads
`$2.4 million` without "point"; the hybrid fixes both classes by never letting the model
near a word it does not need to see.

Call rate and added latency were measured on 400 consecutive pieces (12,509 words) of a
public-domain novel (Pride and Prejudice, Project Gutenberg) split at 400 characters, with
the model server warm. 129 of 400 pieces (32%) contained at least one default or flagged
word and made a model request; narrowing the abbreviation rule (short capitalised words
that merely end a sentence are no longer flagged) moved this from 33% to 32%. The median
piece costs 0.3 ms with the hybrid against 0.2 ms with rules only; a piece that calls the
model has a median cost of 136 ms (p95 of all pieces 200 ms), so a chapter pays roughly a
third of the per-piece latency of the model-only pass.

Homograph accuracy on the 100 eval sentences of the ten table words, hybrid mode with the
same model (an unchanged sense passes as long as the target word itself survives, so
"30-minute" spoken as "thirty minute" still counts):

| Word | bass | dove | lead | live | minute | read | refuse | resume | sake | wound | overall |
|---|---|---|---|---|---|---|---|---|---|---|---|
| Correct | 10/10 | 5/6 | 8/11 | 6/10 | 9/10 | 5/13 | 8/10 | 10/10 | 7/10 | 9/10 | 77/100 (77%) |

Sense decisions on real Wikipedia sentences remain the hardest part for a 4B model: most
misses are a present-tense read or the verb live respelled when they should have stayed as
written, or a rare sense (the past of dive, garbage refuse, the drink sake) left
unchanged. A larger model on the same endpoint will usually do better; the harness is in
place to measure it.

## Limits

Heteronyms with no usable respelling (`wind`, `tear`, `bow`) are left unchanged on purpose,
since rewriting them would trade one wrong pronunciation for another. The year heuristic
reads any four-digit number from 1100 to 2099 as a year unless a plural noun follows, so
"1500 households" stays a count while "in 1500" becomes "fifteen hundred"; the model can
override either way. `350°` and other degree readings, version numbers and unknown
abbreviations are always the model's to decide, which means they stay as written when no
server is configured.
