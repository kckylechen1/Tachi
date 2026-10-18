//! Compile-time provider env-name registry: the admitted-secret vocabulary
//! (`API_KEY_DEFS`) and the name-keyed views derived from it.
//!
//! Lives under `provider_config` (not `status_ops`) so vault, bootstrap, and
//! status code can all classify env-var names without depending upward on the
//! status layer. Pure static data; no I/O.

use std::collections::HashSet;

/// Registry-wide class of an admitted env-var-name secret. This is the
/// compile-time security boundary #1680/D3 introduces: `ModelApi` keys are
/// the only names eligible for the LLM provider materialization allowlist
/// (`model_provider_env_names()`); every class is eligible for the broader
/// "does this name belong to Tachi's provider vocabulary at all" surfaces
/// (lane env injection, providers-doctor admission, the plaintext secret
/// scanner — `admitted_env_secret_names()`). Widening which names are
/// `ModelApi` is a code-review-gated change, never a DB write (D3: "the
/// env-name admission surface stays compile-time").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyClass {
    /// LLM / embedding / reranking provider credentials — eligible for LLM
    /// provider-secret materialization.
    ModelApi,
    /// Web-search provider credentials (Exa, Tavily, Google Programmable
    /// Search) — never materialized into the LLM provider cache.
    SearchApi,
    /// Reserved for non-provider infrastructure secrets. No current
    /// `API_KEY_DEFS` entry uses this class; kept so the class vocabulary
    /// does not need another compile-time change when one shows up.
    #[allow(dead_code)]
    Infra,
}

/// The documented, anti-SSRF-safe probe target for one provider family.
///
/// #1680/D6: this used to be a second hardcoded copy of the host/endpoint
/// strings that `tachi_llm`'s `auth_probe` already had. It is now the same
/// table — the registry's rows reference `tachi_llm`'s compile-time
/// descriptors. The direction is forced by the crate graph (`tachi-llm` must
/// never depend on `tachi-server`) and is the right one anyway: the strings
/// live next to the code that dials them, and what the registry contributes is
/// the part it owns — *which env-var names* are probeable, and as which family.
/// `auth_probe_descriptor_for_env_name` is that lookup;
/// `registry_probe_targets_match_the_probe_table` pins the two against drift.
pub(crate) type ProbeDescriptor = tachi_llm::ProviderProbeDescriptor;

pub(crate) struct ApiKeyDef {
    pub(crate) key: &'static str,
    pub(crate) label: &'static str,
    pub(crate) required: bool,
    pub(crate) deprecated: bool,
    pub(crate) canonical_key: &'static str,
    pub(crate) aliases: &'static [&'static str],
    /// ModelApi vs SearchApi vs Infra — see [`KeyClass`].
    pub(crate) class: KeyClass,
    /// Canonical family id for the underlying account/vendor (e.g.
    /// "deepseek", "anthropic", "google"), independent of which of this
    /// entry's env-var names holds the secret. This is the vocabulary
    /// #1680 D2's `fp1` and D5's account rows are keyed by; reconcile reads
    /// it through `provider_kind_for_env_name`.
    pub(crate) provider_kind: &'static str,
    /// `Some` only for the families `auth_probe` has an owner-verified,
    /// non-generating probe target for. Read through
    /// `auth_probe_descriptor_for_env_name`, which is what makes
    /// the probe surface registry-driven rather than a second host list.
    pub(crate) probe: Option<&'static ProbeDescriptor>,
}

pub(crate) const API_KEY_DEFS: &[ApiKeyDef] = &[
    ApiKeyDef {
        key: "VOYAGE_API_KEY",
        label: "Voyage embeddings",
        required: true,
        deprecated: false,
        canonical_key: "VOYAGE_API_KEY",
        aliases: &[],
        class: KeyClass::ModelApi,
        provider_kind: "voyage",
        probe: None,
    },
    ApiKeyDef {
        key: "VOYAGE_RERANK_API_KEY",
        label: "Voyage rerank",
        required: false,
        deprecated: false,
        canonical_key: "VOYAGE_RERANK_API_KEY",
        aliases: &["VOYAGE_API_KEY"],
        class: KeyClass::ModelApi,
        provider_kind: "voyage",
        probe: None,
    },
    ApiKeyDef {
        key: "SILICONFLOW_API_KEY",
        label: "SiliconFlow/Qwen background LLM",
        required: true,
        deprecated: false,
        canonical_key: "SILICONFLOW_API_KEY",
        aliases: &["EXTRACT_API_KEY", "SUMMARY_API_KEY"],
        class: KeyClass::ModelApi,
        provider_kind: "siliconflow",
        probe: Some(&tachi_llm::SILICONFLOW_AUTH_PROBE),
    },
    // #1680/D2: `REASONING_API_KEY` used to sit in this alias list as well,
    // which made it the one env-var name in the registry that resolved to two
    // provider families — "deepseek" through this row, "zai" through its own
    // row below (whose accepted aliases are `ZAI_API_KEY`/`BIGMODEL_API_KEY`,
    // and which is how the shipped configs actually use the name:
    // `${vault:ZAI_API_KEY|BIGMODEL_API_KEY|REASONING_API_KEY}` in
    // `builtins::mcp`). `aliases` here means "another name for this account",
    // which is what `provider_kind` and D2's `fp1` are keyed by; it is not the
    // lane resolver's fallback chain. That chain
    // (DEEPSEEK → REASONING → ZAI → …) lives in
    // `tachi_llm::llm::provider_health::config` and is unchanged — a reasoning
    // lane still falls back to a Z.AI secret, it just no longer claims the
    // DeepSeek *account* is configured because a Z.AI key exists.
    // `DISTILL_API_KEY` stays: it is genuinely the same DeepSeek family
    // (its own row also declares `provider_kind: "deepseek"`).
    ApiKeyDef {
        key: "DEEPSEEK_API_KEY",
        label: "DeepSeek OpenAI-compatible LLM",
        required: false,
        deprecated: false,
        canonical_key: "DEEPSEEK_API_KEY",
        aliases: &["DISTILL_API_KEY"],
        class: KeyClass::ModelApi,
        provider_kind: "deepseek",
        probe: Some(&tachi_llm::DEEPSEEK_AUTH_PROBE),
    },
    ApiKeyDef {
        key: "DISTILL_API_KEY",
        label: "Distill API lane override before provider fallbacks",
        required: false,
        deprecated: false,
        canonical_key: "DISTILL_API_KEY",
        aliases: &[],
        class: KeyClass::ModelApi,
        provider_kind: "deepseek",
        probe: None,
    },
    ApiKeyDef {
        key: "ZAI_API_KEY",
        label: "Zhipu/BigModel OpenAI-compatible LLM",
        required: false,
        deprecated: false,
        canonical_key: "ZAI_API_KEY",
        aliases: &["BIGMODEL_API_KEY"],
        class: KeyClass::ModelApi,
        provider_kind: "zai",
        // auth_probe recognizes two hosts for this family
        // (open.bigmodel.cn, api.z.ai) with no documented non-generating GET
        // endpoint for either; this names the current primary domain, whose
        // descriptor carries `endpoint: None` — recognized, never probed.
        probe: Some(&tachi_llm::ZAI_AUTH_PROBE),
    },
    // #1355: the grok/xai opencode lane provider must be a recognized
    // provider-key name so an unlocked-vault xAI secret is injected into the
    // lane subprocess env (via `load_unlocked_provider_env_secrets`, gated by
    // `admitted_env_secret_names()`, #1680/D3). That lets `opencode.json`'s `xai`
    // provider use `{env:XAI_API_KEY}` substitution — no literal secret on
    // disk for vault "收权" to blank. `GROK_API_KEY` is carried as an alias
    // (alternate ecosystem name, cf. GOOGLE/GEMINI) so whichever name the
    // owner stored the vault secret under is admitted by the filter; the
    // injected env-var name is always the vault entry's own name, so that
    // name must match the `{env:...}` reference in opencode.json.
    ApiKeyDef {
        key: "XAI_API_KEY",
        label: "xAI/Grok OpenAI-compatible LLM",
        required: false,
        deprecated: false,
        canonical_key: "XAI_API_KEY",
        aliases: &["GROK_API_KEY"],
        class: KeyClass::ModelApi,
        provider_kind: "xai",
        probe: None,
    },
    // #1355(b): the opencode `zhipuai-coding-plan` GLM lane provider must
    // also be a recognized provider-key name so an unlocked-vault ZHIPUAI
    // secret is injected into the lane subprocess env (via
    // `load_unlocked_provider_env_secrets`, gated by
    // `admitted_env_secret_names()`, #1680/D3). That lets `opencode.json`'s
    // `zhipuai-coding-plan` provider use `{env:ZHIPUAI_API_KEY}`
    // substitution — no literal secret on disk for vault "收权" to blank.
    // This is deliberately separate from the `ZAI_API_KEY` entry above:
    // the vault stores a distinct `ZHIPUAI_API_KEY` secret (verified by SHA
    // match against the working opencode literal) that is NOT the same
    // value as `ZAI_API_KEY`, so no alias is added either direction.
    ApiKeyDef {
        key: "ZHIPUAI_API_KEY",
        label: "Zhipu AI (GLM coding-plan) OpenAI-compatible LLM",
        required: false,
        deprecated: false,
        canonical_key: "ZHIPUAI_API_KEY",
        aliases: &[],
        class: KeyClass::ModelApi,
        provider_kind: "zhipuai",
        probe: None,
    },
    // #1355 follow-up: the `kimi-for-coding`/K3 opencode lane provider must
    // also be a recognized provider-key name so an unlocked-vault Kimi
    // secret is injected into the lane subprocess env (via
    // `load_unlocked_provider_env_secrets`, gated by
    // `admitted_env_secret_names()`, #1680/D3). That lets the lane's provider config
    // use `{env:KIMI_API_KEY}` substitution (or direct env read) — no
    // literal secret on disk for vault "收权" to blank. `MOONSHOT_API_KEY`
    // is carried as an alias (alternate ecosystem name — Moonshot AI is
    // Kimi's vendor) so whichever name the owner stored the vault secret
    // under is admitted by the filter; the injected env-var name is always
    // the vault entry's own name, so that name must match the lane's
    // `{env:...}` reference.
    ApiKeyDef {
        key: "KIMI_API_KEY",
        label: "Kimi/Moonshot OpenAI-compatible LLM",
        required: false,
        deprecated: false,
        canonical_key: "KIMI_API_KEY",
        aliases: &["MOONSHOT_API_KEY"],
        class: KeyClass::ModelApi,
        provider_kind: "kimi",
        probe: None,
    },
    ApiKeyDef {
        key: "OPENAI_API_KEY",
        label: "OpenAI-compatible agents",
        required: false,
        deprecated: false,
        canonical_key: "OPENAI_API_KEY",
        aliases: &[],
        class: KeyClass::ModelApi,
        provider_kind: "openai",
        probe: None,
    },
    ApiKeyDef {
        key: "ANTHROPIC_API_KEY",
        label: "Anthropic/Claude-compatible agents",
        required: false,
        deprecated: false,
        canonical_key: "ANTHROPIC_API_KEY",
        aliases: &[],
        class: KeyClass::ModelApi,
        provider_kind: "anthropic",
        probe: None,
    },
    ApiKeyDef {
        key: "GOOGLE_API_KEY",
        label: "Google/Gemini-compatible agents",
        required: false,
        deprecated: false,
        canonical_key: "GOOGLE_API_KEY",
        aliases: &["GEMINI_API_KEY"],
        class: KeyClass::ModelApi,
        provider_kind: "google",
        probe: None,
    },
    ApiKeyDef {
        key: "EXA_API_KEY",
        label: "Exa search",
        required: false,
        deprecated: false,
        canonical_key: "EXA_API_KEY",
        aliases: &[],
        class: KeyClass::SearchApi,
        provider_kind: "exa",
        probe: None,
    },
    ApiKeyDef {
        key: "TAVILY_API_KEY",
        label: "Tavily search",
        required: false,
        deprecated: false,
        canonical_key: "TAVILY_API_KEY",
        aliases: &[],
        class: KeyClass::SearchApi,
        provider_kind: "tavily",
        probe: None,
    },
    // #1680/D3: independent SearchApi entry — previously this name was not
    // in `API_KEY_DEFS` at all, but was still manually folded into the
    // "google/gemini" family by `intake::alias_family()` (a live
    // discrimination-2 violation: a search-only credential grouped with
    // model-provider accounts). Giving it its own registry row with no
    // aliases makes it independent by construction: it is admitted (all
    // classes are admitted-set members) but never enters the ModelApi
    // materialization allowlist, and it no longer shares an intake alias
    // family with GOOGLE_API_KEY/GEMINI_API_KEY.
    ApiKeyDef {
        key: "GOOGLE_SEARCH_API_KEY",
        label: "Google Programmable Search",
        required: false,
        deprecated: false,
        canonical_key: "GOOGLE_SEARCH_API_KEY",
        aliases: &[],
        class: KeyClass::SearchApi,
        provider_kind: "google-search",
        probe: None,
    },
    ApiKeyDef {
        key: "MINIMAX_API_KEY",
        label: "MiniMax legacy distill",
        required: false,
        deprecated: true,
        canonical_key: "DEEPSEEK_API_KEY",
        aliases: &[],
        class: KeyClass::ModelApi,
        provider_kind: "deepseek",
        probe: None,
    },
    ApiKeyDef {
        key: "REASONING_API_KEY",
        label: "Reasoning API lane after DeepSeek and before provider fallbacks",
        required: false,
        deprecated: false,
        canonical_key: "REASONING_API_KEY",
        aliases: &["ZAI_API_KEY", "BIGMODEL_API_KEY"],
        class: KeyClass::ModelApi,
        provider_kind: "zai",
        probe: None,
    },
];

/// Stable internal API for `provider_config`'s LLM materialization allowlist:
/// every `KeyClass::ModelApi` env-var name (primary keys plus aliases)
/// recognized by the status layer, flattened into a set. #1680/D3: this is
/// the *narrow* view — SearchApi (Exa/Tavily/Google Search) and any future
/// Infra names are deliberately excluded, because this set gates which names
/// are eligible to enter the LLM provider secret cache
/// (`materialize_provider_secrets_from_durable_source`). Use
/// [`admitted_env_secret_names`] for "is this name in Tachi's provider
/// vocabulary at all" surfaces (lane env injection, providers-doctor
/// admission, the plaintext secret scanner) — those must see every class.
pub(crate) fn model_provider_env_names() -> HashSet<String> {
    let mut names = HashSet::new();
    for def in API_KEY_DEFS {
        if def.class != KeyClass::ModelApi {
            continue;
        }
        names.insert(def.key.to_string());
        for alias in def.aliases {
            names.insert((*alias).to_string());
        }
    }
    names
}

/// Stable internal API for the all-class "is this a name Tachi's provider
/// registry recognizes" surfaces: lane env injection
/// (`vault_ops::env::load_unlocked_provider_env_secrets`), providers-doctor
/// admission, and the plaintext secret scanner. Unlike
/// [`model_provider_env_names`], this includes every [`KeyClass`] — a
/// Search/Infra key must still be recognized as an admitted provider secret
/// name for those consumers (env injection must keep delivering rotated
/// search keys to lane subprocesses; the scanner must still flag a plaintext
/// search key on disk), it just must never reach the LLM materialization
/// allowlist.
pub(crate) fn admitted_env_secret_names() -> HashSet<String> {
    let mut names = HashSet::new();
    for def in API_KEY_DEFS {
        names.insert(def.key.to_string());
        for alias in def.aliases {
            names.insert((*alias).to_string());
        }
    }
    names
}

/// The canonical provider family an admitted env-var name belongs to, or
/// `None` if the name is not in the registry at all.
///
/// This is the third derived view over `API_KEY_DEFS`, and the one #1680 D2's
/// fingerprints are keyed by: `fp1` mixes in the *provider kind*, never an
/// env-var name, which is exactly what lets two names of one family holding one
/// secret collapse into a single account while the same value under two
/// different vendors stays uncorrelated. Resolution prefers an exact primary-key
/// match and falls back to the alias tables, and
/// `provider_kind_assignment_is_unambiguous` pins that no name can resolve two
/// ways — an ambiguous name would silently split or merge accounts depending on
/// registry declaration order.
///
/// Consumed by #1680 PR-C's reconcile pipeline; declared here, with the other
/// two derived views, because a fourth private copy of "which family is this
/// name" is precisely the drift D3 deleted `intake::alias_family` to end.
pub(crate) fn provider_kind_for_env_name(name: &str) -> Option<&'static str> {
    registry_def_for_env_name(name).map(|def| def.provider_kind)
}

/// The account class an admitted env-var name belongs to — the stored form of
/// the `KeyClass` boundary D3 introduced, so an account row keeps saying
/// "search credential" after the process that read the registry is gone.
///
/// Returns `None` for a name the registry does not recognize at all, which is
/// the answer reconcile needs: an unrecognized name is not eligible to become
/// an account of any class.
pub(crate) fn account_class_for_env_name(name: &str) -> Option<memcore::AccountClass> {
    registry_def_for_env_name(name).map(|def| match def.class {
        KeyClass::ModelApi => memcore::AccountClass::ModelApi,
        KeyClass::SearchApi => memcore::AccountClass::SearchApi,
        KeyClass::Infra => memcore::AccountClass::Infra,
    })
}

/// The canonical primary key of the registry entry an admitted name belongs
/// to. Reconcile uses it to pick which of a group's interchangeable names is
/// the one custody points at, so that choice is the registry's rather than an
/// accident of iteration order.
pub(crate) fn canonical_key_for_env_name(name: &str) -> Option<&'static str> {
    registry_def_for_env_name(name).map(|def| def.canonical_key)
}

/// Every interchangeable env-var name of the registry entry an admitted name
/// belongs to — the entry's own key plus its aliases — or `None` when the name
/// is not in the registry.
///
/// The external Vault tests use this projection to check alias-family behavior
/// without exposing the internal registry row type through the test API.
#[cfg(feature = "vault-test-api")]
pub(crate) fn family_env_names_for_env_name(name: &str) -> Option<Vec<&'static str>> {
    registry_def_for_env_name(name).map(|def| {
        std::iter::once(def.key)
            .chain(def.aliases.iter().copied())
            .collect()
    })
}

/// The documented authentication-probe target for an admitted env-var name,
/// or `None` when the name is unknown to the registry or its family has no
/// owner-verified, non-generating probe endpoint (#1680 D6).
///
/// This is the view that makes probing registry-driven: a caller holding a
/// vault logical name asks the registry "may this be probed, and as what",
/// and hands the answer to `tachi_llm`'s
/// `LlmClient::probe_member_auth_and_record`. The *hosts* stay compile-time
/// constants in `tachi_llm` — widening what a credential-bearing request may
/// dial is a code change in the module that dials it, never a registry edit
/// and never a DB write. What the registry decides is only which names are in
/// scope, which is exactly the half it owns.
///
/// Alias names resolve through their family, so probing
/// `EXTRACT_API_KEY` probes SiliconFlow, as it should.
pub(crate) fn auth_probe_descriptor_for_env_name(
    name: &str,
) -> Option<&'static tachi_llm::ProviderProbeDescriptor> {
    registry_def_for_env_name(name).and_then(|def| def.probe)
}

/// The single registry lookup the name-keyed views above share: primary-key
/// match first (never shadowed by another entry that merely lists the name as
/// an alias), then the alias tables.
fn registry_def_for_env_name(name: &str) -> Option<&'static ApiKeyDef> {
    API_KEY_DEFS
        .iter()
        .find(|def| def.key == name)
        .or_else(|| API_KEY_DEFS.iter().find(|def| def.aliases.contains(&name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xai_and_zai_are_recognized_provider_env_names() {
        // #1355: the grok/xai opencode lane provider uses `{env:XAI_API_KEY}`
        // substitution in opencode.json so vault "收权" can never blank it (no
        // literal on disk). That only works end-to-end if this name is in the
        // provider-key filter that gates which unlocked-vault secrets get
        // injected into the lane subprocess env
        // (`load_unlocked_provider_env_secrets` -> `admitted_env_secret_names`,
        // #1680/D3).
        let names = admitted_env_secret_names();
        assert!(
            names.contains("XAI_API_KEY"),
            "XAI_API_KEY must be a recognized provider env name so an unlocked-vault \
             xAI secret materializes into the grok lane child env"
        );
        // Alternate ecosystem name is admitted too (whichever the owner stored).
        assert!(
            names.contains("GROK_API_KEY"),
            "GROK_API_KEY alias must be admitted by the provider-key filter"
        );
        // Zhipu/BigModel family already covered before #1355.
        assert!(names.contains("ZAI_API_KEY"));
        assert!(names.contains("BIGMODEL_API_KEY"));
    }

    #[test]
    fn zhipuai_is_a_recognized_provider_env_name() {
        // #1355(b): the opencode `zhipuai-coding-plan` GLM lane provider uses
        // `{env:ZHIPUAI_API_KEY}` substitution in opencode.json. `ZAI_API_KEY`
        // (asserted above) does NOT cover this — the vault stores a distinct
        // `ZHIPUAI_API_KEY` secret (verified by SHA match against the working
        // opencode literal at `zhipuai-coding-plan.options.apiKey`) holding a
        // different value than `ZAI_API_KEY`. Without `ZHIPUAI_API_KEY` in the
        // provider-key filter, the GLM lane subprocess never receives its
        // vault secret and `{env:ZHIPUAI_API_KEY}` resolves to nothing.
        let names = admitted_env_secret_names();
        assert!(
            names.contains("ZHIPUAI_API_KEY"),
            "ZHIPUAI_API_KEY must be a recognized provider env name so an unlocked-vault \
             zhipuai secret materializes into the opencode GLM lane child env"
        );
    }

    #[test]
    fn kimi_is_a_recognized_provider_env_name() {
        // #1355 follow-up: the `kimi-for-coding`/K3 opencode lane provider uses
        // `{env:KIMI_API_KEY}` substitution (or direct env read) so vault
        // "收权" can never blank it (no literal on disk). That only works
        // end-to-end if this name is in the provider-key filter that gates
        // which unlocked-vault secrets get injected into the lane subprocess
        // env (`load_unlocked_provider_env_secrets` -> `admitted_env_secret_names`,
        // #1680/D3).
        let names = admitted_env_secret_names();
        assert!(
            names.contains("KIMI_API_KEY"),
            "KIMI_API_KEY must be a recognized provider env name so an unlocked-vault \
             Kimi secret materializes into the kimi-for-coding lane child env"
        );
        // Alternate ecosystem name is admitted too (whichever the owner stored;
        // Moonshot AI is Kimi's vendor).
        assert!(
            names.contains("MOONSHOT_API_KEY"),
            "MOONSHOT_API_KEY alias must be admitted by the provider-key filter"
        );
    }

    /// #1680/D3 env-injection regression, codex NEEDS-FIXES BUG-3: the legacy
    /// comparison set below is a **hardcoded literal**, copied by hand from
    /// `git show 6800f09d:crates/tachi-server/src/status_ops/status_health/api_keys.rs`
    /// (the frozen base commit this whole PR branches from) — every primary key
    /// and every alias in that pre-#1680 `API_KEY_DEFS`, flattened. It is
    /// deliberately NOT derived from `HEAD`'s `API_KEY_DEFS` (that would let a
    /// regression prove itself correct by re-deriving its own expected answer
    /// from the same registry it just changed).
    ///
    /// `admitted_env_secret_names()` (the surface that gates lane env injection,
    /// providers-doctor admission, and the plaintext secret scanner) is asserted
    /// to equal that frozen legacy set **plus exactly one new name**:
    /// `GOOGLE_SEARCH_API_KEY`. That growth is an intended, explicit consequence
    /// of this PR (#1680/D3: `GOOGLE_SEARCH_API_KEY` becomes its own independent
    /// SearchApi registry entry instead of being unreachable outside intake's
    /// old alias table) — not an accidental widening. If lane env injection ever
    /// admits anything beyond that one explicit addition, this test goes red.
    #[test]
    fn admitted_env_secret_names_matches_frozen_base_legacy_set_plus_google_search() {
        let legacy_set_at_6800f09d: HashSet<String> = [
            "VOYAGE_API_KEY",
            "VOYAGE_RERANK_API_KEY",
            "SILICONFLOW_API_KEY",
            "EXTRACT_API_KEY",
            "SUMMARY_API_KEY",
            "DEEPSEEK_API_KEY",
            "DISTILL_API_KEY",
            "REASONING_API_KEY",
            "ZAI_API_KEY",
            "BIGMODEL_API_KEY",
            "XAI_API_KEY",
            "GROK_API_KEY",
            "ZHIPUAI_API_KEY",
            "KIMI_API_KEY",
            "MOONSHOT_API_KEY",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "GOOGLE_API_KEY",
            "GEMINI_API_KEY",
            "EXA_API_KEY",
            "TAVILY_API_KEY",
            "MINIMAX_API_KEY",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert_eq!(
            legacy_set_at_6800f09d.len(),
            22,
            "the hardcoded legacy literal itself must have 22 unique names — a \
             miscount here would silently weaken this discriminator"
        );

        let mut expected_admitted_set = legacy_set_at_6800f09d.clone();
        expected_admitted_set.insert("GOOGLE_SEARCH_API_KEY".to_string());

        assert_eq!(
            admitted_env_secret_names(),
            expected_admitted_set,
            "env-injection admitted set must grow by exactly GOOGLE_SEARCH_API_KEY \
             over the frozen pre-#1680 base — no other name may appear or vanish"
        );

        // And the narrowed materialization view must be a strict subset that
        // drops at least the SearchApi names — never equal to the admitted set,
        // or the split accomplished nothing.
        assert!(model_provider_env_names().is_subset(&admitted_env_secret_names()));
        assert!(model_provider_env_names().len() < admitted_env_secret_names().len());
    }

    /// #1680 D2/D3: the fingerprint domain input must be unambiguous.
    ///
    /// `fp1` is keyed by `provider_kind`, so if one env-var name could resolve to
    /// two different kinds — say `BIGMODEL_API_KEY` reading as `zai` through one
    /// registry row and as something else through another — the same secret would
    /// fingerprint two ways depending on which row a caller happened to match, and
    /// accounts would split or merge by declaration order. Every admitted name is
    /// checked against every row it appears in, as a primary key or as an alias.
    #[test]
    fn provider_kind_assignment_is_unambiguous_across_the_registry() {
        for name in admitted_env_secret_names() {
            let kinds: HashSet<&'static str> = API_KEY_DEFS
                .iter()
                .filter(|def| def.key == name || def.aliases.contains(&name.as_str()))
                .map(|def| def.provider_kind)
                .collect();
            assert_eq!(
                kinds.len(),
                1,
                "env name {name} resolves to {kinds:?}; a name may belong to exactly one provider \
                 family or #1680's key fingerprints become order-dependent"
            );
            assert_eq!(
                provider_kind_for_env_name(&name),
                kinds.into_iter().next(),
                "the derived view must agree with the registry rows it reads"
            );
        }
    }

    /// The alias half, spelled out on the case that motivates it: the two names
    /// that hold one DeepSeek account's key resolve to one family, and an unknown
    /// name resolves to nothing rather than to a guess.
    #[test]
    fn provider_kind_resolves_aliases_and_refuses_unknown_names() {
        assert_eq!(
            provider_kind_for_env_name("DEEPSEEK_API_KEY"),
            Some("deepseek")
        );
        assert_eq!(
            provider_kind_for_env_name("DISTILL_API_KEY"),
            Some("deepseek")
        );
        assert_eq!(provider_kind_for_env_name("GEMINI_API_KEY"), Some("google"));
        assert_eq!(provider_kind_for_env_name("MOONSHOT_API_KEY"), Some("kimi"));
        assert_eq!(provider_kind_for_env_name("NOT_A_PROVIDER_KEY"), None);
        assert_eq!(provider_kind_for_env_name(""), None);
    }

    /// The discrimination-2 boundary restated in the fingerprint domain: search
    /// credentials must not share a provider family with the model accounts they
    /// were historically folded into (`intake::alias_family` put
    /// `GOOGLE_SEARCH_API_KEY` in the google/gemini family). Same family would mean
    /// same fingerprint domain, which would put a search key one merge away from a
    /// model account.
    #[test]
    fn search_keys_get_their_own_provider_kinds() {
        assert_eq!(
            provider_kind_for_env_name("GOOGLE_SEARCH_API_KEY"),
            Some("google-search")
        );
        assert_ne!(
            provider_kind_for_env_name("GOOGLE_SEARCH_API_KEY"),
            provider_kind_for_env_name("GOOGLE_API_KEY")
        );
        assert_eq!(provider_kind_for_env_name("EXA_API_KEY"), Some("exa"));
        assert_eq!(provider_kind_for_env_name("TAVILY_API_KEY"), Some("tavily"));
    }

    /// #1680 D6: there is one probe-target table, and it is `tachi_llm`'s. The
    /// registry's job is to say *which env-var names* may be probed and as which
    /// family; the hosts and endpoints themselves stay compile-time constants in
    /// the module that dials them, because that is the anti-SSRF boundary. This
    /// pins the two halves against the drift the duplicated table used to invite.
    #[test]
    fn registry_probe_targets_match_the_probe_table() {
        for def in API_KEY_DEFS {
            let Some(descriptor) = def.probe else {
                continue;
            };
            assert_eq!(
                descriptor.provider_kind, def.provider_kind,
                "{} points at a {} probe target",
                def.key, descriptor.provider_kind
            );
            assert!(
                tachi_llm::AUTH_PROBE_DESCRIPTORS.contains(descriptor),
                "{} points at a descriptor outside the probe table",
                def.key
            );
            assert_eq!(
                tachi_llm::auth_probe_descriptor_for_host(descriptor.host),
                Some(descriptor),
                "{} names a host the probe table does not admit",
                def.key
            );
        }

        // The other direction: a host the probe table is willing to dial that no
        // registry entry can reach would be an unreachable exception to the
        // admission boundary.
        for descriptor in tachi_llm::AUTH_PROBE_DESCRIPTORS {
            assert!(
                API_KEY_DEFS
                    .iter()
                    .any(|def| def.provider_kind == descriptor.provider_kind),
                "probe table host {} belongs to no registry family",
                descriptor.host
            );
        }
    }

    /// The probe surface is per env-var name, alias names included, and it is
    /// closed: a name the registry does not recognize, or a family with no
    /// documented non-generating endpoint, is never probeable.
    #[test]
    fn auth_probe_targets_resolve_by_env_name_and_refuse_everything_else() {
        assert_eq!(
            auth_probe_descriptor_for_env_name("DEEPSEEK_API_KEY").and_then(|probe| probe.endpoint),
            Some("https://api.deepseek.com/models")
        );
        // An alias is another name for the same account, so it probes the same
        // family.
        assert_eq!(
            auth_probe_descriptor_for_env_name("EXTRACT_API_KEY"),
            auth_probe_descriptor_for_env_name("SILICONFLOW_API_KEY")
        );
        assert_eq!(
            auth_probe_descriptor_for_env_name("SILICONFLOW_API_KEY")
                .map(|probe| probe.provider_kind),
            Some("siliconflow")
        );
        // Recognized family, no documented probe: probeable-by-name, never dialed.
        assert_eq!(
            auth_probe_descriptor_for_env_name("ZAI_API_KEY").map(|probe| probe.endpoint),
            Some(None)
        );
        // Search credentials and unknown names have no probe target at all.
        assert_eq!(auth_probe_descriptor_for_env_name("EXA_API_KEY"), None);
        assert_eq!(auth_probe_descriptor_for_env_name("TAVILY_API_KEY"), None);
        assert_eq!(
            auth_probe_descriptor_for_env_name("NOT_A_PROVIDER_KEY"),
            None
        );
        assert_eq!(auth_probe_descriptor_for_env_name(""), None);
    }
}
