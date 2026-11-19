# Text normalization

An optional pass that fixes the words a synthesis engine misreads before they are spoken: firearm
calibers (`.45s`, `5.56`), money (`$9.50`), years (`1984`), clock times (`3:30`), roman numerals in
chapter headings and after monarchs, titles (`Dr.`, `Sgt.`, `vs.`), and words such as a past-tense
`read` or the metal `lead`. Each piece of a chapter is sent to a language model, which returns a
spoken form for the few words that need one. Everything runs through any OpenAI-compatible chat
endpoint, local or hosted.

## Turning it on

In Settings, open "Pronunciation helper (experimental)" below the voice settings, tick Enabled and
fill in three fields: the server address (default
`http://127.0.0.1:8091/v1`), the model name, and an optional API key. The key is passed to the
worker process in the `INKVOICE_NORMALIZE_KEY` environment variable and sent as a bearer token, so
it never appears on a command line. Any server that implements `POST /v1/chat/completions` works:
llama.cpp's `llama-server`, LM Studio, vLLM, or a hosted provider.

For a local setup, serve a small instruct model with llama.cpp, for example:

```
llama-server -m <model>.gguf -ngl 99 -c 4096 --host 127.0.0.1 --port 8091 --jinja
```

The pass is off when the URL is empty, and the workers then behave exactly as before.

## The word-for-word guarantee

Word timings, resume and highlighting are keyed on character offsets of the original text, so the
pass can never add, drop or reorder words. For every piece it returns exactly one spoken token per
original word, and most words come back unchanged.

A replacement keeps only the original word's opening quotes or brackets in front, and only sentence
punctuation, closing quotes or brackets and dashes behind. A leading decimal point, `$`, `%` or `°`
is part of the spoken thing and goes with the replacement, and an abbreviation's own period is
dropped (`Dr.` mid-sentence becomes `Doctor`, with the sentence's final period kept when the word
ends the piece). Every guarantee is enforced in code in `workers/normalizer.py`; the model only
picks the words and their spoken forms. A spoken form may contain only letters, spaces, hyphens and
apostrophes, at most eight words, no digits.

## Fallbacks

When anything goes wrong, the pass returns the original words:

- A reply that is not valid JSON, uses an out-of-range word number, or changes more than a quarter
  of a piece's words (two words minimum) is discarded for that piece.
- Network errors and timeouts return the original words. After three consecutive failed requests
  the endpoint is not called again for the rest of the worker's life, so a hung server costs at
  most three timeouts per chapter instead of one per piece.
- Each kind of failure prints at most one line on the worker's stderr.

The request asks for schema-constrained JSON output and disables model thinking where supported;
servers that reject those fields are retried without them.

## Cache

Successful pieces are cached in `<InkVoice data>/normalize-cache/`, one small JSON file per piece,
keyed by prompt version, model name and the piece text. Regenerating a chapter, or streaming it a
second time, makes no calls to the endpoint at all. Delete the folder to redo everything; the cache
writes are atomic and a failed write is ignored.

## Running the evaluation

`tools/normalize_eval/cases.jsonl` holds 76 original sentences with the expected spoken forms,
grouped by category (calibers, decimals, money, dates, clock times, ordinals, abbreviations,
respellable heteronyms, and controls that must not change). With a server running:

```
python tools/normalize_eval/run_eval.py --url http://127.0.0.1:8091/v1 --model <name>
```

The eval disables the cache and reports, per category, the share of expected changes made
correctly, false changes on words that should not change, invalid or discarded replies, and latency
p50/p95 per piece. Spoken forms are compared case-insensitively with hyphens treated as spaces.

## Results

Measured on one RTX 4070 SUPER (12 GB) with llama.cpp, context 4096, all layers on the GPU, prompt
revision 5 (2026-11). GPU memory is the amount in use while serving, above an otherwise idle
desktop. 661 words, 64 expected changes, 76 sentences.

Share of expected changes made correctly, per category:

| Category | Qwen3-4B-Instruct-2507 | Llama-3.1-8B-Instruct | gemma-3-4b-it |
|---|---|---|---|
| Abbreviations and ranks | 100% | 80% | 60% |
| Calibers and gauges | 100% | 100% | 71% |
| Dates and years | 100% | 56% | 56% |
| Decimals and measurements | 100% | 80% | 50% |
| Heteronyms (read, lead, live, wound) | 100% | 62% | 25% |
| Money | 75% | 88% | 50% |
| Ordinals and roman numerals | 100% | 71% | 86% |
| Clock times | 80% | 80% | 80% |

| | Qwen3-4B-Instruct-2507 | Llama-3.1-8B-Instruct | gemma-3-4b-it |
|---|---|---|---|
| Quantization, file size | Q4_K_M, 2.5 GB | Q4_K_M, 4.9 GB | Q4_K_M, 2.5 GB |
| Expected changes correct | 61 of 64 (95%) | 49 of 64 (77%) | 37 of 64 (58%) |
| False changes (of 661 words) | 4 | 11 | 22 |
| Invalid or discarded replies | 0 | 1 | 11 |
| Latency per piece, p50 / p95 | 123 ms / 187 ms | 185 ms / 291 ms | 238 ms / 471 ms |
| GPU memory in use | about 3.3 GB | about 5.3 GB | about 3.2 GB |

Qwen3-4B-Instruct-2507 is the recommended model: highest accuracy, fewest false changes, lowest
latency, and about half the memory and file size of the 8B.

## Limits

Heteronyms with no usable respelling (`wind`, `tear`, `bow`) are left unchanged on purpose, since
rewriting them would trade one wrong pronunciation for another. With the recommended 4B model the
remaining misses are rare calibers such as `.30-06`, amounts like `$2.4 million` (sometimes spoken
without "point"), and adjacent number-plus-abbreviation pairs such as `6 a.m.` that the model
occasionally merges into one change. A larger model on the same endpoint will usually do better;
the prompt and all guards stay the same.
