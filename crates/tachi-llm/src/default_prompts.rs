// Default prompts for Tachi's high-level LLM helpers. Callers that need
// provider-generic chat paths use the lane APIs without these defaults.

pub const EXTRACTION_PROMPT: &str = r#"你是一个记忆提取代理。从对话/文档中提取值得**长期记忆**的离散事实。

输出 JSON 数组，每个元素:
- "text": 一条自洽的事实，简洁但**完整**——保留后续拼接推理所需的关键证据、中间状态与操作顺序，只删除纯口语词（"刚才/之前/舰长"等）。不要为求短而切掉过程
- "topic": 主题标签
- "keywords": 2-5个关键词/标签
- "entities": 人名、产品、仓库、模块、组织等实体，没有则 []
- "scope": "user" / "project" / "general"
- "importance": 0.0-1.0

核心规则:
1) 合并同类：同一根因的多个描述合并为一条，但不同根因保留为独立事实
2) 保留证据：不要只留结论而丢掉导向结论的关键证据、过程或前提——这些在后续多步推理拼接时是必需的（晚过滤优于早过滤）
3) 宁全勿损：不要为凑数硬拆，也不要为求"少"而合并不同主题或删除有信息量的细节；一段话通常1-3条，技术上独立的问题不应强行合并
4) 不编造，仅输出 JSON 数组"#;

pub const SUMMARY_PROMPT: &str = "You are a summarization agent. Compress the given text into a single precisely worded sentence that captures the core fact or point. Do not use conversational filler, quotes, or markdown. Use the same language as the input text.";

pub const METADATA_EXTRACTION_PROMPT: &str = r#"Extract searchable metadata from one memory entry. Output JSON only:
{"keywords": ["2-5 topical tags"], "entities": ["proper nouns, tickers, repos, modules, people"]}

Rules:
- keywords = recall tags (topics, concepts, actions); entities = named things (688981, hyperion, repo-name)
- Use the same language as the input text
- Do not invent facts absent from the text
- Empty arrays are allowed when nothing applies"#;

/// Write-side synonym + bilingual keyword expansion for FTS recall (#921).
/// Used only when `TACHI_WRITE_ENRICH_KEYWORDS` is enabled.
pub const KEYWORD_ENRICHMENT_PROMPT: &str = r#"Generate synonym and bilingual (Chinese↔English) search keywords for one memory entry to widen FTS recall. Output JSON only:
{"keywords": ["8-16 short search terms"]}

Rules:
- Include synonyms, related terms, acronyms, and zh↔en translations of key concepts present in the text
- Prefer high-precision recall tags; avoid stopwords and generic noise
- Keep terms short (1-4 words, or short CJK phrases)
- Do not invent facts absent from the text
- Prefer new terms that extend (not merely repeat) any existing keywords provided
- Empty array is allowed when nothing useful applies"#;
