# PhotoCraft retouch

Guides with screenshots: [繁體中文](../../docs/generative-retouch/README.md) · [English](../../docs/generative-retouch/README.en.md) · [日本語](../../docs/generative-retouch/README.ja.md).

A small program that runs beside PhotoCraft. It holds the Gemini API key and the page where the prompt and aspect ratio are chosen. PhotoCraft only sends the selection and, later, places the returned image on a new layer.

## Using it

PhotoCraft starts this program when you choose **Edit › Generative Retouch**, if it was installed next to PhotoCraft. Installers and the portable zip do not contain a Gemini API key, and they do not read a `.env` file or `GEMINI_API_KEY` from the environment. The page asks you to paste a key from [Google AI Studio](https://aistudio.google.com/apikey) and press **Save key**. That saved key is the one later launches use. A portable copy stores it in `PhotoCraftData` next to the program; an installed copy stores it in the PhotoCraft settings folder. PhotoCraft never sees it.

You can also start it yourself:

- **Windows:** Start menu › **PhotoCraft Retouch**. Leave the window open.
- **Linux:** the **PhotoCraft Retouch** application entry (it opens a terminal window; leave that window open).

Closing the window stops retouch.

The Flatpak build does not grant network access, which Gemini needs. One-time setup:

```sh
flatpak override --user --share=network ai.storyteller.photocraft
```

## Developers

`cargo run` (a debug build) still reads `GEMINI_API_KEY` from `.env` (see `.env.example`) so a developer can call Gemini without pasting, and that value wins over a key saved from the page. Release builds do not: `cargo run --release`, installers, and the portable zip.

```sh
cargo run -p photocraft-gemini-retouch
```

Logs are English. Put `--` before flags so Cargo forwards them:

```sh
cargo run -p photocraft-gemini-retouch -- --lang ja
cargo run -p photocraft-gemini-retouch -- --lang zh-Hant
cargo run -p photocraft-gemini-retouch -- --no-browser
```

`en` (the default), `ja`, and `zh-Hant` (also `zh-TW`) choose the log language and the prompt page. `--help` prints the same choices. PhotoCraft passes the language its window is using, which is the system language unless Preferences choose another, and it passes `--no-browser`: PhotoCraft opens one page after the selection is uploaded. The JSON the editor reads stays in English. A helper you start yourself still opens the page at startup. `--open-on-selection` waits to open that page until the first selection arrives.

The server listens on `http://127.0.0.1:8787/` (override with `RETOUCH_ADDR`, loopback only).

## In PhotoCraft

1. Draw a selection in one of Gemini's aspect ratios (Rectangular Marquee, Fixed Ratio: 1:1, 16:9, 4:3, 3:2, 4:5, 9:16, 21:9, and the other ratios in the list on the page). One pixel of rounding still counts.
2. Edit › Generative Retouch. That uploads the selection and opens this page.
3. Pick the ratio if you want a different one from the list, type a prompt (for example “remove stray hair”, “retouch skin”, or “remove passerby”) and press Send. The page also has those three as buttons. A different ratio is placed in the largest rectangle of that shape centered on the selection.
4. The page shows the generated image. **Use this** adds it to PhotoCraft as a new layer. **Regenerate** runs the same prompt again. **Show original** compares it with the selection you sent.
5. Edit › Place Generative Result still adds the latest image if you prefer the menu.

Agents can skip the page by passing the prompt on the command:

`edit.generativeRetouch` with `{"prompt":"remove stray hair","aspectRatio":"16:9"}`.

The model is `gemini-nano-banana-2.1`. The aspect ratio is one of `1:1`, `1:4`, `1:8`, `2:3`, `3:2`, `3:4`, `4:1`, `4:3`, `4:5`, `5:4`, `8:1`, `9:16`, `16:9`, `21:9`. The output tier is the smallest of 1K, 2K and 4K whose frame is at least as large as the region being placed. A selection larger than that ratio's 4K frame is refused.
