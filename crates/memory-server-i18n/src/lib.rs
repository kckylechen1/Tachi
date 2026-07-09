//! Minimal i18n string lookup (`t()`) with en/zh locales, resolved from the
//! `TACHI_LOCALE` env var. Extracted from memory-server (#833 slice 2).

#![allow(clippy::manual_pattern_char_comparison)]

use std::collections::HashMap;
use std::sync::OnceLock;

type LocaleMap = HashMap<&'static str, HashMap<&'static str, &'static str>>;

fn translations() -> &'static LocaleMap {
    static T: OnceLock<LocaleMap> = OnceLock::new();
    T.get_or_init(|| {
        let mut en = HashMap::new();
        en.insert("vault.locked", "Vault is locked. Run vault_unlock.");
        en.insert("vault.not_initialized", "Vault is not initialized.");
        en.insert(
            "vault.access_denied",
            "Access denied: agent not authorized.",
        );
        en.insert("vault.secret_not_found", "Secret not found.");
        en.insert("db.busy", "Database is busy. Retrying...");
        en.insert("db.error", "Database error.");
        en.insert("search.no_results", "No results found.");
        en.insert("dispatch.failed", "Dispatch failed.");
        en.insert("config.invalid", "Invalid configuration.");
        en.insert("health.degraded", "Service degraded.");

        let mut zh = HashMap::new();
        zh.insert("vault.locked", "保险库已锁定。请运行 vault_unlock。");
        zh.insert("vault.not_initialized", "保险库未初始化。");
        zh.insert("vault.access_denied", "访问被拒绝：代理未授权。");
        zh.insert("vault.secret_not_found", "未找到密钥。");
        zh.insert("db.busy", "数据库繁忙。正在重试…");
        zh.insert("db.error", "数据库错误。");
        zh.insert("search.no_results", "未找到结果。");
        zh.insert("dispatch.failed", "调度失败。");
        zh.insert("config.invalid", "配置无效。");
        zh.insert("health.degraded", "服务降级。");

        let mut map = HashMap::new();
        map.insert("en", en);
        map.insert("zh", zh);
        map
    })
}

fn current_locale() -> &'static str {
    static LOCALE: OnceLock<String> = OnceLock::new();
    LOCALE
        .get_or_init(|| {
            std::env::var("TACHI_LOCALE")
                .unwrap_or_default()
                .to_lowercase()
                .split(|c: char| c == '_' || c == '-')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or("en")
                .to_string()
        })
        .as_str()
}

pub fn t(key: &str) -> String {
    let locale = current_locale();
    translations()
        .get(locale)
        .and_then(|m| m.get(key))
        .or_else(|| translations().get("en").and_then(|m| m.get(key)))
        .copied()
        .unwrap_or(key)
        .to_string()
}

pub fn available_locales() -> Vec<&'static str> {
    translations().keys().copied().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_to_en_for_unknown_locale() {
        std::env::set_var("TACHI_LOCALE", "fr");
        let val = t("vault.locked");
        assert!(val.contains("Vault is locked"));
        std::env::remove_var("TACHI_LOCALE");
    }

    #[test]
    fn returns_key_for_unknown_translation() {
        let val = t("nonexistent.key.12345");
        assert_eq!(val, "nonexistent.key.12345");
    }

    #[test]
    fn available_locales_includes_en_and_zh() {
        let locales = available_locales();
        assert!(locales.contains(&"en"));
        assert!(locales.contains(&"zh"));
    }

    #[test]
    fn known_keys_resolve_for_en() {
        // Touch every catalog key so locale tables are not "dead" under -D dead_code.
        for key in [
            "vault.locked",
            "vault.not_initialized",
            "vault.access_denied",
            "vault.secret_not_found",
            "db.busy",
            "db.error",
            "search.no_results",
            "dispatch.failed",
            "config.invalid",
            "health.degraded",
        ] {
            let val = t(key);
            assert_ne!(val, key, "missing en translation for {key}");
        }
    }
}
