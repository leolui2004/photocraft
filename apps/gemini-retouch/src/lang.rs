//! Operator-facing log language. English is the default. Japanese and Traditional Chinese
//! cover the same stderr lines. HTTP responses and the prompt page stay in English, because
//! PhotoCraft and agents match those strings.

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
}

/// `--lang en|ja|zh-Hant`. Unknown flags are an error so a typo is not a silent English log.
pub fn parse_args<S: AsRef<str>>(args: &[S]) -> Result<Options, String> {
    let mut lang = Lang::En;
    let mut help = false;
    let mut open_browser = true;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_ref();
        if arg == "--help" || arg == "-h" {
            help = true;
        } else if arg == "--no-browser" {
            open_browser = false;
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
    Ok(Options { lang, help, open_browser })
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
    pub fn help(self) -> &'static str {
        match self {
            Lang::En => {
                "\
photocraft-gemini-retouch [--lang en|ja|zh-Hant] [--no-browser]

  (default)          English logs, and a browser window
  --lang ja          Japanese logs
  --lang zh-Hant     Traditional Chinese logs
  --no-browser       Do not open a browser
  --help             Show this help
"
            }
            Lang::Ja => {
                "\
photocraft-gemini-retouch [--lang en|ja|zh-Hant] [--no-browser]

  （既定）           英語のログとブラウザ
  --lang ja          日本語のログ
  --lang zh-Hant     繁体字中国語のログ
  --no-browser       ブラウザを開かない
  --help             このヘルプを表示
"
            }
            Lang::ZhHant => {
                "\
photocraft-gemini-retouch [--lang en|ja|zh-Hant] [--no-browser]

  （預設）           英文日誌，並開啟瀏覽器
  --lang ja          日文日誌
  --lang zh-Hant     繁體中文日誌
  --no-browser       不要開啟瀏覽器
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
            Lang::En => "Square selection in PhotoCraft, then Edit › Generative Retouch. Enter the prompt on this page.",
            Lang::Ja => "PhotoCraft で正方形を選択し、Edit › Generative Retouch を選んでください。プロンプトはこのページに入力します。",
            Lang::ZhHant => "請在 PhotoCraft 中框選正方形，然後選擇 Edit › Generative Retouch。請在此頁面輸入提示。",
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
            Lang::Ja => "PhotoCraft Retouch のページに Gemini API キーを貼り付けて Save を押してください。キーは https://aistudio.google.com/apikey で取得できます",
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
            Lang::En => format!("unknown argument `{arg}` (expected --lang en|ja|zh-Hant or --no-browser)"),
            Lang::Ja => format!("不明な引数 `{arg}` です（--lang en|ja|zh-Hant または --no-browser を指定してください）"),
            Lang::ZhHant => format!("不明的參數 `{arg}`（請使用 --lang en|ja|zh-Hant 或 --no-browser）"),
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
        assert!(!parse_args(&["--no-browser"]).unwrap().open_browser);

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
    }
}
