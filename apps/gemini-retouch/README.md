# PhotoCraft retouch

A small program that runs beside PhotoCraft. It holds the Gemini API key and the page where the prompt is typed. PhotoCraft only sends the selected square and, later, places the returned image on a new layer.

## Using it

PhotoCraft starts this program when you choose **Edit › Generative Retouch**, if it was installed next to PhotoCraft. The first time, the page asks you to paste a Gemini API key from [Google AI Studio](https://aistudio.google.com/apikey) and press **Save key**. The key stays on this computer.

You can also start it yourself:

- **Windows:** Start menu › **PhotoCraft Retouch**. Leave the window open.
- **Linux:** the **PhotoCraft Retouch** application entry (it opens a terminal window; leave that window open).

Closing the window stops retouch.

The Flatpak build does not grant network access, which Gemini needs. One-time setup:

```sh
flatpak override --user --share=network ai.storyteller.photocraft
```

## Developers

From the repository root, a key in `.env` (see `.env.example`) is still read, and it wins over a key saved from the page:

```sh
cargo run -p photocraft-gemini-retouch
```

Logs are English. Put `--` before flags so Cargo forwards them:

```sh
cargo run -p photocraft-gemini-retouch -- --lang ja
cargo run -p photocraft-gemini-retouch -- --lang zh-Hant
cargo run -p photocraft-gemini-retouch -- --no-browser
```

`en` (the default), `ja`, and `zh-Hant` (also `zh-TW`) choose the log language. `--help` prints the same choices. The prompt page and the JSON the editor reads stay in English. `--no-browser` skips opening a browser; PhotoCraft passes it when it starts this program itself.

The server listens on `http://127.0.0.1:8787/` (override with `RETOUCH_ADDR`, loopback only).

## In PhotoCraft

1. Draw a square selection (Rectangular Marquee with equal width and height).
2. Edit › Generative Retouch. That uploads the square and opens this page.
3. Type a prompt (for example “remove stray hair” or “retouch skin”) and press Send.
4. Edit › Place Generative Result. The image becomes a new layer covering the selection.

Agents can skip the page by passing the prompt on the command:

`edit.generativeRetouch` with `{"prompt":"remove stray hair"}`.

The model is `gemini-nano-banana-2.1`, aspect ratio `1:1`. The output tier is the smallest of 1K (1024), 2K (2048) and 4K (4096) that is at least as large as the selection. A square larger than 4096 px is refused, because that is the largest 1:1 frame the model can return.
