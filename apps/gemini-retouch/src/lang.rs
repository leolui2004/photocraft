//! Operator-facing language. English is the default. Japanese and Traditional Chinese cover the
//! same stderr lines and the prompt page. HTTP JSON stays in English, because PhotoCraft and
//! agents match those strings.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Lang {
    #[default]
    En,
    Ja,
    ZhHant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageLabel {
    Input,
    Output,
    Thinking,
    Cached,
    Tool,
    Total,
    Unknown,
}

#[derive(Debug)]
pub struct Options {
    pub lang: Lang,
    pub help: bool,
    /// Open the prompt page in a browser once the server is listening.
    pub open_browser: bool,
    /// Open that page after the first selection is stored, instead of at startup.
    /// PhotoCraft passes this so the page's first view already shows the selection.
    pub open_on_selection: bool,
}

/// `--lang en|ja|zh-Hant`. Unknown flags are an error so a typo is not a silent English log.
pub fn parse_args<S: AsRef<str>>(args: &[S]) -> Result<Options, String> {
    let mut lang = Lang::En;
    let mut help = false;
    let mut open_browser = true;
    let mut open_on_selection = false;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_ref();
        if arg == "--help" || arg == "-h" {
            help = true;
        } else if arg == "--no-browser" {
            open_browser = false;
        } else if arg == "--open-on-selection" {
            open_on_selection = true;
        } else if arg == "--lang" || arg == "--language" {
            i += 1;
            let Some(value) = args.get(i).map(|s| s.as_ref()) else {
                return Err(lang.err_lang_missing().to_string());
            };
            lang = parse_lang_code(value).map_err(|()| lang.err_unknown_lang(value))?;
        } else if let Some(value) = arg.strip_prefix("--lang=").or_else(|| arg.strip_prefix("--language=")) {
            lang = parse_lang_code(value).map_err(|()| lang.err_unknown_lang(value))?;
        } else {
            return Err(lang.err_unknown_arg(arg));
        }
        i += 1;
    }
    Ok(Options { lang, help, open_browser, open_on_selection })
}

fn parse_lang_code(raw: &str) -> Result<Lang, ()> {
    let key = raw.trim();
    if key.eq_ignore_ascii_case("en") || key.eq_ignore_ascii_case("english") {
        Ok(Lang::En)
    } else if key.eq_ignore_ascii_case("ja") || key.eq_ignore_ascii_case("jp") || key.eq_ignore_ascii_case("japanese") {
        Ok(Lang::Ja)
    } else if key.eq_ignore_ascii_case("zh-hant")
        || key.eq_ignore_ascii_case("zh_hant")
        || key.eq_ignore_ascii_case("zh-tw")
        || key.eq_ignore_ascii_case("zht")
        || key.eq_ignore_ascii_case("traditional-chinese")
    {
        Ok(Lang::ZhHant)
    } else {
        Err(())
    }
}

impl Lang {
    /// `en`, `ja`, or `zh-Hant`. Unknown text is `None` (a typo must not become a silent English page).
    pub fn from_code(raw: &str) -> Option<Self> {
        parse_lang_code(raw).ok()
    }

    /// Path segment used when opening the prompt page (`/ja`).
    pub fn page_code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Ja => "ja",
            Lang::ZhHant => "zh-Hant",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Lang::En => {
                "\
photocraft-gemini-retouch [--lang en|ja|zh-Hant] [--no-browser] [--open-on-selection]

  (default)          English logs and prompt page, and a browser window
  --lang ja          Japanese logs and prompt page
  --lang zh-Hant     Traditional Chinese logs and prompt page
  --no-browser       Do not open a browser
  --open-on-selection  Open the browser after the first selection arrives
  --help             Show this help
"
            }
            Lang::Ja => {
                "\
photocraft-gemini-retouch [--lang en|ja|zh-Hant] [--no-browser] [--open-on-selection]

  （既定）           英語のログとプロンプトページ、およびブラウザ
  --lang ja          日本語のログとプロンプトページ
  --lang zh-Hant     繁体字中国語のログとプロンプトページ
  --no-browser       ブラウザを開かない
  --open-on-selection  最初の選択範囲が届いてからブラウザを開く
  --help             このヘルプを表示
"
            }
            Lang::ZhHant => {
                "\
photocraft-gemini-retouch [--lang en|ja|zh-Hant] [--no-browser] [--open-on-selection]

  （預設）           英文日誌與提示頁，並開啟瀏覽器
  --lang ja          日文日誌與提示頁
  --lang zh-Hant     繁體中文日誌與提示頁
  --no-browser       不要開啟瀏覽器
  --open-on-selection  第一次收到選取範圍後再開啟瀏覽器
  --help             顯示此說明
"
            }
        }
    }

    pub fn banner_listening(self, addr: &impl std::fmt::Display) -> String {
        match self {
            Lang::En => format!("PhotoCraft retouch backend at http://{addr}/"),
            Lang::Ja => format!("PhotoCraft レタッチバックエンド: http://{addr}/"),
            Lang::ZhHant => format!("PhotoCraft 潤飾後端: http://{addr}/"),
        }
    }

    pub fn banner_howto(self) -> &'static str {
        match self {
            Lang::En => "Select a Gemini ratio (1:1, 16:9, …) in PhotoCraft, then Edit › Generative Retouch. Pick the ratio and enter the prompt on this page.",
            Lang::Ja => {
                "PhotoCraft で Gemini の縦横比（1:1、16:9 など）を選択し、編集 › 生成レタッチ を選んでください。縦横比とプロンプトはこのページで指定します。"
            }
            Lang::ZhHant => "請在 PhotoCraft 中選取 Gemini 長寬比（1:1、16:9 等），然後選擇「編輯 › 生成式潤飾」。請在此頁面選擇長寬比並輸入提示。",
        }
    }

    pub fn banner_leave_open(self) -> &'static str {
        match self {
            Lang::En => "Leave this window open. Closing it stops retouch.",
            Lang::Ja => "このウィンドウは開いたままにしてください。閉じるとレタッチが止まります。",
            Lang::ZhHant => "請保持此視窗開啟。關閉後潤飾會停止。",
        }
    }

    pub fn err_missing_key(self) -> &'static str {
        match self {
            Lang::En => "Paste a Gemini API key on the PhotoCraft Retouch page and press Save. Get a key at https://aistudio.google.com/apikey",
            Lang::Ja => {
                "PhotoCraft Retouch のページに Gemini API キーを貼り付けて Save を押してください。キーは https://aistudio.google.com/apikey で取得できます"
            }
            Lang::ZhHant => "請在 PhotoCraft Retouch 頁面貼上 Gemini API 金鑰並按 Save。金鑰可在 https://aistudio.google.com/apikey 取得",
        }
    }

    pub fn err_bad_addr(self, addr: &str) -> String {
        match self {
            Lang::En => format!("RETOUCH_ADDR `{addr}` is not a host:port address"),
            Lang::Ja => format!("RETOUCH_ADDR `{addr}` は host:port 形式のアドレスではありません"),
            Lang::ZhHant => format!("RETOUCH_ADDR `{addr}` 不是 host:port 位址"),
        }
    }

    pub fn err_not_loopback(self) -> &'static str {
        match self {
            Lang::En => "RETOUCH_ADDR must be a loopback address (127.0.0.1). The API key stays on this machine.",
            Lang::Ja => "RETOUCH_ADDR はループバックアドレス（127.0.0.1）にしてください。API キーはこのマシンに残します。",
            Lang::ZhHant => "RETOUCH_ADDR 必須是迴路位址（127.0.0.1）。API 金鑰只留在這台機器上。",
        }
    }

    pub fn err_listen(self, addr: &impl std::fmt::Display, err: &impl std::fmt::Display) -> String {
        match self {
            Lang::En => format!("could not listen on {addr}: {err}"),
            Lang::Ja => format!("{addr} で待ち受けできませんでした: {err}"),
            Lang::ZhHant => format!("無法在 {addr} 監聽: {err}"),
        }
    }

    pub fn err_thread(self, err: &impl std::fmt::Display) -> String {
        match self {
            Lang::En => format!("could not start the server thread: {err}"),
            Lang::Ja => format!("サーバースレッドを開始できませんでした: {err}"),
            Lang::ZhHant => format!("無法啟動伺服器執行緒: {err}"),
        }
    }

    pub fn log_accept_failed(self, err: &impl std::fmt::Display) -> String {
        match self {
            Lang::En => format!("retouch backend: accept failed: {err}"),
            Lang::Ja => format!("レタッチバックエンド: 接続の受け付けに失敗しました: {err}"),
            Lang::ZhHant => format!("潤飾後端: 接受連線失敗: {err}"),
        }
    }

    pub fn log_backend(self, err: &impl std::fmt::Display) -> String {
        match self {
            Lang::En => format!("retouch backend: {err}"),
            Lang::Ja => format!("レタッチバックエンド: {err}"),
            Lang::ZhHant => format!("潤飾後端: {err}"),
        }
    }

    pub fn err_unknown_arg(self, arg: &str) -> String {
        match self {
            Lang::En => format!("unknown argument `{arg}` (expected --lang en|ja|zh-Hant, --no-browser, or --open-on-selection)"),
            Lang::Ja => format!("不明な引数 `{arg}` です（--lang en|ja|zh-Hant、--no-browser、または --open-on-selection を指定してください）"),
            Lang::ZhHant => format!("不明的參數 `{arg}`（請使用 --lang en|ja|zh-Hant、--no-browser 或 --open-on-selection）"),
        }
    }

    pub fn err_lang_missing(self) -> &'static str {
        match self {
            Lang::En => "--lang needs a value: en, ja, or zh-Hant",
            Lang::Ja => "--lang には値（en、ja、zh-Hant）が必要です",
            Lang::ZhHant => "--lang 需要一個值：en、ja 或 zh-Hant",
        }
    }

    pub fn err_unknown_lang(self, value: &str) -> String {
        match self {
            Lang::En => format!("unknown language `{value}` (expected en, ja, or zh-Hant)"),
            Lang::Ja => format!("不明な言語 `{value}` です（en、ja、zh-Hant のいずれかを指定してください）"),
            Lang::ZhHant => format!("不明的語言 `{value}`（請使用 en、ja 或 zh-Hant）"),
        }
    }

    pub fn usage_missing(self, image_size: &str, fields: &str) -> String {
        match self {
            Lang::En => format!("Gemini usage ({image_size}): not reported (response fields: {fields})"),
            Lang::Ja => format!("Gemini 使用量 ({image_size}): 報告なし（応答フィールド: {fields}）"),
            Lang::ZhHant => format!("Gemini 用量 ({image_size}): 未回報（回應欄位: {fields}）"),
        }
    }

    pub fn usage_body(self, image_size: &str, body: &str) -> String {
        match self {
            Lang::En => format!("Gemini usage ({image_size}): {body}"),
            Lang::Ja => format!("Gemini 使用量 ({image_size}): {body}"),
            Lang::ZhHant => format!("Gemini 用量 ({image_size}): {body}"),
        }
    }

    pub fn usage_label(self, label: UsageLabel) -> &'static str {
        match label {
            UsageLabel::Input => match self {
                Lang::En => "input",
                Lang::Ja => "入力",
                Lang::ZhHant => "輸入",
            },
            UsageLabel::Output => match self {
                Lang::En => "output",
                Lang::Ja => "出力",
                Lang::ZhHant => "輸出",
            },
            UsageLabel::Thinking => match self {
                Lang::En => "thinking",
                Lang::Ja => "思考",
                Lang::ZhHant => "思考",
            },
            UsageLabel::Cached => match self {
                Lang::En => "cached",
                Lang::Ja => "キャッシュ",
                Lang::ZhHant => "快取",
            },
            UsageLabel::Tool => match self {
                Lang::En => "tool",
                Lang::Ja => "ツール",
                Lang::ZhHant => "工具",
            },
            UsageLabel::Total => match self {
                Lang::En => "total",
                Lang::Ja => "合計",
                Lang::ZhHant => "總計",
            },
            UsageLabel::Unknown => match self {
                Lang::En => "unknown",
                Lang::Ja => "不明",
                Lang::ZhHant => "未知",
            },
        }
    }

    /// Copy for the prompt page. Japanese and Traditional Chinese menu names match PhotoCraft's catalogs.
    pub fn page(self) -> PageText {
        match self {
            Lang::En => PageText {
                html_lang: "en",
                title: "PhotoCraft retouch",
                intro: "In PhotoCraft, select a region in one of Gemini's ratios (a square, 16:9, 4:3, and the others in the list) and choose Edit › Generative Retouch. Pick the ratio, describe the change, and send it. When the image looks right, press Use this. Regenerate tries again.",
                key_before: "Paste the API key from ",
                key_link: "Google AI Studio",
                key_after: ". It stays on this computer. PhotoCraft never sees it.",
                api_key: "API key",
                save_key: "Save key",
                empty: "No selection yet.",
                prompt_hint: "remove stray hair",
                hair: "Remove stray hair",
                skin: "Retouch skin",
                passerby: "Remove passerby",
                ratio: "Aspect ratio",
                send: "Send",
                use_this: "Use this",
                again: "Regenerate",
                show_original: "Show original",
                show_result: "Show result",
                adding: "Adding it to PhotoCraft…",
                added: "Added to PhotoCraft.",
                confirm_failed: "Could not confirm the image.",
                menu_fallback: "Confirmed. If it does not appear in PhotoCraft, choose Edit › Place Generative Result.",
                result_alt: "Generated image",
                saving: "Saving the key…",
                save_failed: "Could not save the key.",
                saved: "API key saved on this computer.",
                unreachable: "Could not reach the backend.",
                alt: "Selected region",
                sending: "Sending to Gemini…",
                ready: "Look at the result. Use this image, or regenerate.",
                failed: "The retouch failed.",
                received: "Selection received ({w}×{h}, {ratio}, output {size}). Enter a prompt and send.",
                need_key: "Paste your Gemini API key above. It is saved on this computer.",
            },
            Lang::Ja => PageText {
                html_lang: "ja",
                title: "PhotoCraft レタッチ",
                intro: "PhotoCraft で Gemini の縦横比（正方形、16:9、4:3 など）に合う範囲を選択し、編集 › 生成レタッチ を選んでください。縦横比を選び、変更内容を書いて送信します。よければ「これを使う」を押し、やり直すときは「再生成」を押します。",
                key_before: "",
                key_link: "Google AI Studio",
                key_after: " の API キーを貼り付けてください。キーはこのパソコンに保存され、PhotoCraft には渡りません。",
                api_key: "API キー",
                save_key: "キーを保存",
                empty: "まだ選択範囲がありません。",
                prompt_hint: "ほつれ毛を消す",
                hair: "ほつれ毛を消す",
                skin: "肌をレタッチ",
                passerby: "通行人を消す",
                ratio: "縦横比",
                send: "送信",
                use_this: "これを使う",
                again: "再生成",
                show_original: "元の画像を表示",
                show_result: "生成結果を表示",
                adding: "PhotoCraft に追加しています…",
                added: "PhotoCraft に追加しました。",
                confirm_failed: "画像を確定できませんでした。",
                menu_fallback: "確定しました。PhotoCraft に出ない場合は、編集 › 生成結果を配置 を選んでください。",
                result_alt: "生成された画像",
                saving: "キーを保存しています…",
                save_failed: "キーを保存できませんでした。",
                saved: "API キーをこのパソコンに保存しました。",
                unreachable: "バックエンドに接続できませんでした。",
                alt: "選択した範囲",
                sending: "Gemini に送信しています…",
                ready: "結果を確認してください。この画像を使うか、再生成してください。",
                failed: "レタッチに失敗しました。",
                received: "選択範囲を受け取りました（{w}×{h}、{ratio}、出力 {size}）。プロンプトを入力して送信してください。",
                need_key: "上の欄に Gemini API キーを貼り付けてください。このパソコンに保存されます。",
            },
            Lang::ZhHant => PageText {
                html_lang: "zh-Hant",
                title: "PhotoCraft 潤飾",
                intro: "請在 PhotoCraft 中框選符合 Gemini 長寬比的範圍（正方形、16:9、4:3 等），然後選擇「編輯 › 生成式潤飾」。選擇長寬比、描述修改並傳送。影像正確時請按「使用這張」。「重新生成」會再試一次。",
                key_before: "請貼上 ",
                key_link: "Google AI Studio",
                key_after: " 的 API 金鑰。金鑰只留在這台電腦上，PhotoCraft 不會看到。",
                api_key: "API 金鑰",
                save_key: "儲存金鑰",
                empty: "尚未收到選取範圍。",
                prompt_hint: "去除多餘的頭髮",
                hair: "去除多餘的頭髮",
                skin: "潤飾皮膚",
                passerby: "去除路人",
                ratio: "長寬比",
                send: "傳送",
                use_this: "使用這張",
                again: "重新生成",
                show_original: "顯示原圖",
                show_result: "顯示結果",
                adding: "正在加入 PhotoCraft…",
                added: "已加入 PhotoCraft。",
                confirm_failed: "無法確認影像。",
                menu_fallback: "已確認。若 PhotoCraft 沒有出現影像，請選擇「編輯 › 置入生成結果」。",
                result_alt: "生成的影像",
                saving: "正在儲存金鑰…",
                save_failed: "無法儲存金鑰。",
                saved: "已將 API 金鑰儲存在這台電腦上。",
                unreachable: "無法連線到後端。",
                alt: "選取的範圍",
                sending: "正在傳送到 Gemini…",
                ready: "請查看結果。使用這張影像，或重新生成。",
                failed: "潤飾失敗。",
                received: "已收到選取範圍（{w}×{h}，{ratio}，輸出 {size}）。請輸入提示並傳送。",
                need_key: "請在上方貼上 Gemini API 金鑰。金鑰會儲存在這台電腦上。",
            },
        }
    }
}

/// Strings the prompt page shows. `{w}`, `{h}` and `{size}` in [`PageText::received`] are filled in by the page.
pub struct PageText {
    pub html_lang: &'static str,
    pub title: &'static str,
    pub intro: &'static str,
    pub key_before: &'static str,
    pub key_link: &'static str,
    pub key_after: &'static str,
    pub api_key: &'static str,
    pub save_key: &'static str,
    pub empty: &'static str,
    pub prompt_hint: &'static str,
    pub hair: &'static str,
    pub skin: &'static str,
    pub passerby: &'static str,
    pub ratio: &'static str,
    pub send: &'static str,
    pub use_this: &'static str,
    pub again: &'static str,
    pub show_original: &'static str,
    pub show_result: &'static str,
    pub adding: &'static str,
    pub added: &'static str,
    pub confirm_failed: &'static str,
    pub menu_fallback: &'static str,
    pub result_alt: &'static str,
    pub saving: &'static str,
    pub save_failed: &'static str,
    pub saved: &'static str,
    pub unreachable: &'static str,
    pub alt: &'static str,
    pub sending: &'static str,
    pub ready: &'static str,
    pub failed: &'static str,
    pub received: &'static str,
    pub need_key: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_english_and_the_three_log_languages_parse() {
        let opts = parse_args::<&str>(&[]).unwrap();
        assert_eq!(opts.lang, Lang::En);
        assert!(!opts.help);
        assert!(opts.open_browser);
        assert!(!opts.open_on_selection);
        assert!(!parse_args(&["--no-browser"]).unwrap().open_browser);
        let delayed = parse_args(&["--lang", "ja", "--open-on-selection"]).unwrap();
        assert_eq!(delayed.lang, Lang::Ja);
        assert!(delayed.open_browser);
        assert!(delayed.open_on_selection);
        assert!(!parse_args(&["--no-browser", "--open-on-selection"]).unwrap().open_browser);

        assert_eq!(parse_args(&["--lang", "ja"]).unwrap().lang, Lang::Ja);
        assert_eq!(parse_args(&["--language", "japanese"]).unwrap().lang, Lang::Ja);
        assert_eq!(parse_args(&["--lang=zh-Hant"]).unwrap().lang, Lang::ZhHant);
        assert_eq!(parse_args(&["--lang", "zh-TW"]).unwrap().lang, Lang::ZhHant);
        assert_eq!(parse_args(&["--lang", "en"]).unwrap().lang, Lang::En);
        assert!(parse_args(&["--help"]).unwrap().help);
        assert_eq!(parse_args(&["--lang", "ja", "--help"]).unwrap().lang, Lang::Ja);
    }

    #[test]
    fn a_bad_language_or_flag_names_the_three_modes() {
        let err = parse_args(&["--lang", "zh-CN"]).unwrap_err();
        assert!(err.contains("zh-Hant"), "{err}");
        let err = parse_args(&["--lang", "ja", "--lang", "fr"]).unwrap_err();
        assert!(err.contains("不明な言語"), "{err}");
        let err = parse_args(&["--nope"]).unwrap_err();
        assert!(err.contains("--lang"), "{err}");
        assert!(parse_args(&["--lang"]).is_err());
    }

    #[test]
    fn banners_and_usage_words_differ_by_language() {
        assert!(Lang::En.banner_listening(&"127.0.0.1:8787").contains("retouch backend"));
        assert!(Lang::Ja.banner_listening(&"127.0.0.1:8787").contains("レタッチバックエンド"));
        assert!(Lang::ZhHant.banner_listening(&"127.0.0.1:8787").contains("潤飾後端"));
        assert_eq!(Lang::Ja.usage_label(UsageLabel::Input), "入力");
        assert_eq!(Lang::ZhHant.usage_label(UsageLabel::Total), "總計");
        assert!(Lang::Ja.help().contains("日本語のログ"));
        assert!(Lang::ZhHant.help().contains("繁體中文日誌"));
        assert_eq!(Lang::from_code("ja"), Some(Lang::Ja));
        assert_eq!(Lang::from_code("zh-hant"), Some(Lang::ZhHant));
        assert_eq!(Lang::Ja.page_code(), "ja");
        assert!(Lang::Ja.page().empty.contains("選択範囲"));
        assert!(Lang::Ja.page().received.contains("{w}"));
        assert!(Lang::Ja.page().received.contains("{ratio}"));
        assert_eq!(Lang::Ja.page().ratio, "縦横比");
        assert_eq!(Lang::ZhHant.page().ratio, "長寬比");
        assert!(Lang::ZhHant.page().title.contains("潤飾"));
        assert!(!Lang::Ja.page().intro.contains("Select a square"));
        assert!(Lang::Ja.page().intro.contains("生成レタッチ"));
        assert!(Lang::Ja.page().again.contains("再生成"));
        assert!(Lang::Ja.page().menu_fallback.contains("生成結果を配置"));
        assert_eq!(Lang::Ja.page().hair, "ほつれ毛を消す");
        assert_eq!(Lang::Ja.page().passerby, "通行人を消す");
        assert_eq!(Lang::En.page().passerby, "Remove passerby");
        assert_eq!(Lang::ZhHant.page().passerby, "去除路人");
        assert!(Lang::Ja.banner_howto().contains("生成レタッチ"));
        assert!(Lang::ZhHant.page().intro.contains("生成式潤飾"));
        assert!(Lang::ZhHant.page().use_this.contains("使用這張"));
        assert!(Lang::ZhHant.page().menu_fallback.contains("置入生成結果"));
        assert!(Lang::ZhHant.banner_howto().contains("生成式潤飾"));
        assert!(!Lang::Ja.page().intro.contains("Generative Retouch"));
        assert!(!Lang::ZhHant.page().ready.contains("Place Generative Result"));
        assert!(!Lang::ZhHant.page().menu_fallback.contains("Place Generative Result"));
    }
}
