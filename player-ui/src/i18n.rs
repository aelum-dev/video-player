//! i18n: locale selection and the message catalogs.
//!
//! Mirrors the installer frontend's i18n (`distro/installer/frontend/src/lib/i18n`),
//! so both halves of the desktop speak the same locale contract: flat dotted
//! keys, `{placeholder}` interpolation, and the same fallback chain — active
//! locale → en-US → the key itself. Catalogs are JSON files in
//! `data/translations/`, embedded at compile time (the same pattern as
//! `data/style.css` in `theme.rs`), so the packaged binary carries them and
//! needs no data directory.
//!
//! `SUPPORTED_LOCALES` is the locale contract, copied from the installer's
//! language list; only en-US has a catalog so far. A supported-but-untranslated
//! locale resolves to English through the per-key fallback, so shipping a
//! translation is a data change: drop in `data/translations/<code>.json` and
//! add it to `CATALOGS`. (RTL locales will also want the window direction set
//! when they arrive — English-only code deliberately does not carry that yet.)
//!
//! This covers the app's own strings only. GTK's chrome (file chooser buttons,
//! sort menus) is localized by GTK itself from the process locale.

use std::collections::HashMap;
use std::sync::OnceLock;

/// The fallback every catalog leans on, and the locale an unusable
/// environment lands on.
const FALLBACK: &str = "en-US";

/// The locales the UI is expected to support — the installer's language list
/// (`distro/installer/frontend/src/translations`), BCP-47 codes keyed the
/// same way the installer keys its JSON files.
const SUPPORTED_LOCALES: &[&str] = &[
    "ar-SA", "bg-BG", "ca-ES", "de-DE", "el-GR", "en-US", "eo", "es-ES", "fa-IR", "fi-FI",
    "fr-FR", "he-IL", "hu-HU", "id-ID", "it-IT", "ja-JP", "ko-KR", "nb-NO", "nl-NL", "pl-PL",
    "pt-BR", "ru-RU", "sv-SE", "tr-TR", "uk-UA", "vi-VN", "zh-CN", "zh-TW",
];

/// The catalogs compiled into the binary. English must be here (it is the
/// fallback); the others appear as translations land.
const CATALOGS: &[(&str, &str)] = &[("en-US", include_str!("../data/translations/en-US.json"))];

/// The process locale, picked up the way glibc would: LC_ALL, then
/// LC_MESSAGES, then LANG.
fn env_locale() -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|var| std::env::var(var).ok())
        .filter(|value| !value.is_empty())
}

/// Normalize a locale string (`de_DE.UTF-8`, `de_DE@formal`, `de-DE`, `eo`)
/// to a BCP-47 code (`de-DE`, `eo`). `C`, `POSIX`, and unparseable values
/// return `None` — the caller treats that as "no preference".
fn normalize_locale(raw: &str) -> Option<String> {
    // Strip the codeset and the modifier (`language[_region][.codeset][@mod]`).
    let base = raw.split('@').next()?.split('.').next()?.trim();
    if base.is_empty() || base == "C" || base == "POSIX" {
        return None;
    }
    let mut parts = base.splitn(2, ['_', '-']);
    let language = parts.next()?.trim();
    if language.is_empty() || !language.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let language = language.to_ascii_lowercase();
    // The region is the first segment after the separator (`de_DE_formal` →
    // `DE`); anything that is not two letters is a modifier, not a region.
    let region = parts
        .next()
        .and_then(|r| r.split('_').next())
        .filter(|r| r.len() == 2 && r.chars().all(|c| c.is_ascii_alphabetic()))
        .map(|r| r.to_ascii_uppercase());
    Some(match region {
        Some(region) => format!("{language}-{region}"),
        None => language,
    })
}

/// Pick the active locale for a raw environment value: an exact match on the
/// supported list, else the first supported variant of the same language
/// (`de-AT` → `de-DE`), else English.
fn select_locale(raw: Option<&str>) -> &'static str {
    if let Some(code) = raw.and_then(normalize_locale) {
        if let Some(supported) = SUPPORTED_LOCALES.iter().find(|l| **l == code) {
            return supported;
        }
        let language = code.split('-').next().unwrap_or("");
        if let Some(supported) = SUPPORTED_LOCALES
            .iter()
            .find(|l| l.split('-').next() == Some(language))
        {
            return supported;
        }
    }
    FALLBACK
}

/// Parse one embedded catalog; a catalog that does not parse is an error log
/// and an empty map (the fallback chain keeps the UI in English).
fn catalog(code: &str) -> HashMap<String, String> {
    CATALOGS
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, json)| match serde_json::from_str::<HashMap<String, String>>(json) {
            Ok(map) => map,
            Err(err) => {
                tracing::error!(%err, code, "translation catalog does not parse");
                HashMap::new()
            }
        })
        .unwrap_or_default()
}

/// The active locale's messages with the English fallback behind them.
struct I18n {
    messages: HashMap<String, String>,
    fallback: HashMap<String, String>,
}

impl I18n {
    fn load(locale: &str) -> I18n {
        let fallback = catalog(FALLBACK);
        let messages = if locale == FALLBACK {
            fallback.clone()
        } else {
            catalog(locale)
        };
        I18n { messages, fallback }
    }

    /// Active locale → en-US → the key itself, like the installer frontend.
    /// The result borrows from the catalogs or from `key` (both outlive it).
    fn t<'a>(&'a self, key: &'a str) -> &'a str {
        self.messages
            .get(key)
            .or_else(|| self.fallback.get(key))
            .map_or(key, String::as_str)
    }

    fn t_fmt(&self, key: &str, args: &[(&str, &str)]) -> String {
        interpolate(self.t(key), args)
    }
}

/// Replace `{name}` placeholders the way the installer frontend does: a
/// placeholder whose argument was not supplied interpolates as empty.
fn interpolate(message: &str, args: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(message.len());
    let mut rest = message;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) if is_name(&after[..close]) => {
                out.push_str(
                    args.iter()
                        .find(|(name, _)| *name == &after[..close])
                        .map_or("", |(_, value)| *value),
                );
                rest = &after[close + 1..];
            }
            // A `{` that does not open a placeholder stays literal.
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Placeholder names are `\w+` — same as the frontend's regex.
fn is_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

static I18N: OnceLock<I18n> = OnceLock::new();

/// Choose the locale from the environment and load the catalogs. Call once
/// at startup, before any string is built.
pub fn init() {
    let requested = env_locale();
    let locale = select_locale(requested.as_deref());
    tracing::info!(locale, requested = requested.as_deref().unwrap_or(""), "ui language");
    let _ = I18N.set(I18n::load(locale));
}

/// Translate a key, no arguments.
pub fn t(key: &str) -> String {
    I18N.get_or_init(|| I18n::load(select_locale(None)))
        .t(key)
        .to_owned()
}

/// Translate a key and interpolate its `{placeholders}`.
pub fn t_fmt(key: &str, args: &[(&str, &str)]) -> String {
    I18N.get_or_init(|| I18n::load(select_locale(None)))
        .t_fmt(key, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_locale_codes_normalize_to_bcp47() {
        assert_eq!(normalize_locale("de_DE.UTF-8").as_deref(), Some("de-DE"));
        assert_eq!(normalize_locale("pt_BR.utf8").as_deref(), Some("pt-BR"));
        assert_eq!(normalize_locale("zh_CN").as_deref(), Some("zh-CN"));
        assert_eq!(normalize_locale("eo.UTF-8").as_deref(), Some("eo"));
        assert_eq!(normalize_locale("en-US").as_deref(), Some("en-US"));
        assert_eq!(normalize_locale("de_DE@formal").as_deref(), Some("de-DE"));
    }

    #[test]
    fn neutral_and_unparseable_locales_are_none() {
        assert_eq!(normalize_locale("C"), None);
        assert_eq!(normalize_locale("POSIX"), None);
        assert_eq!(normalize_locale(""), None);
        assert_eq!(normalize_locale("123"), None);
    }

    #[test]
    fn selection_matches_the_supported_list() {
        assert_eq!(select_locale(Some("de_DE.UTF-8")), "de-DE");
        assert_eq!(select_locale(Some("pt_BR.UTF-8")), "pt-BR");
        assert_eq!(select_locale(Some("eo")), "eo");
    }

    #[test]
    fn unsupported_variants_resolve_to_the_same_language() {
        assert_eq!(select_locale(Some("de_AT.UTF-8")), "de-DE");
        assert_eq!(select_locale(Some("en_GB.UTF-8")), "en-US");
        assert_eq!(select_locale(Some("zh_Hans_CN.UTF-8")), "zh-CN");
    }

    #[test]
    fn unknown_locales_land_on_english() {
        assert_eq!(select_locale(Some("xx_XX.UTF-8")), "en-US");
        assert_eq!(select_locale(Some("C")), "en-US");
        assert_eq!(select_locale(None), "en-US");
    }

    #[test]
    fn untranslated_locales_fall_back_to_english_per_key() {
        // de-DE is supported but has no catalog yet: every key resolves
        // through English, exactly like the installer frontend.
        let i18n = I18n::load("de-DE");
        assert_eq!(i18n.t("app.name"), "Video player");
        assert_eq!(i18n.t("no.such.key"), "no.such.key");
    }

    #[test]
    fn placeholders_interpolate_and_missing_args_are_empty() {
        let i18n = I18n::load("en-US");
        assert_eq!(
            i18n.t_fmt(
                "toast.openFailed",
                &[("path", "/tmp/a.mkv"), ("error", "no such file")]
            ),
            "Could not open /tmp/a.mkv: no such file"
        );
        assert_eq!(
            i18n.t_fmt("toast.openFailed", &[("path", "/tmp/a.mkv")]),
            "Could not open /tmp/a.mkv: "
        );
    }

    #[test]
    fn a_brace_that_opens_no_placeholder_stays_literal() {
        // Unclosed, and an empty name: neither is a `{name}` placeholder.
        assert_eq!(interpolate("100% {ok", &[]), "100% {ok");
        assert_eq!(interpolate("{} x", &[]), "{} x");
    }

    #[test]
    fn the_english_catalog_defines_every_key_the_ui_asks_for() {
        // A missing key shows up on screen as the raw key — catch that here
        // instead. Keep this list in step with the t()/t_fmt() call sites.
        let json = CATALOGS
            .iter()
            .find(|(code, _)| *code == FALLBACK)
            .expect("the English catalog is embedded")
            .1;
        let english: HashMap<String, String> =
            serde_json::from_str(json).expect("en-US.json parses");
        for key in [
            "app.name",
            "bar.fullscreen",
            "bar.mute",
            "bar.speed",
            "bar.volume",
            "center.next",
            "center.prev",
            "dialog.open.title",
            "drop.hint",
            "header.close",
            "header.open",
            "menu.fullscreen",
            "menu.label",
            "menu.open",
            "menu.quit",
            "toast.dndExtra",
            "toast.noGl",
            "toast.openFailed",
            "toast.playbackFailed",
        ] {
            assert!(english.contains_key(key), "en-US.json is missing {key}");
        }
    }
}
