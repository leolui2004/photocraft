# PhotoCraft retouch backend

A small local server, separate from the PhotoCraft app. It holds the Gemini API key and the prompt form. PhotoCraft only sends the selected square and, later, places the returned image on a new layer.

## Run

From the repository root, put your key in `.env` (see `.env.example`):

```sh
cargo run -p photocraft-gemini-retouch
```

Logs are English. Put `--` before `--lang` so Cargo forwards the flag:

```sh
cargo run -p photocraft-gemini-retouch -- --lang ja
cargo run -p photocraft-gemini-retouch -- --lang zh-Hant
```

`en` (the default), `ja`, and `zh-Hant` (also `zh-TW`) choose the log language. `--help` prints the same choices. The prompt page and the JSON the editor reads stay in English.

The server listens on `http://127.0.0.1:8787/` (override with `RETOUCH_ADDR`, loopback only).

## In PhotoCraft

1. Draw a square selection (Rectangular Marquee with equal width and height).
2. Edit › Generative Retouch. That uploads the square and opens this page.
3. Type a prompt (for example “remove stray hair” or “retouch skin”) and press Send.
4. Edit › Place Generative Result. The image becomes a new layer covering the selection.

Agents can skip the page by passing the prompt on the command:

`edit.generativeRetouch` with `{"prompt":"remove stray hair"}`.

The model is `gemini-nano-banana-2.1`, aspect ratio `1:1`. The output tier is the smallest of 1K (1024), 2K (2048) and 4K (4096) that is at least as large as the selection. A square larger than 4096 px is refused, because that is the largest 1:1 frame the model can return.
