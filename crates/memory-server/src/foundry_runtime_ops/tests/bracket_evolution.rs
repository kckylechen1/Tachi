use super::*;
#[test]
fn extract_bracket_self_evolution_notes_filters_and_dedups() {
    let messages = vec![
        Message {
            role: "assistant".to_string(),
            content: "你好（普通感想）还有（原来他喜欢这种直球，记住了，下次我要先夸再问）"
                .to_string(),
        },
        Message {
            role: "assistant".to_string(),
            content: "(原来他喜欢这种直球，记住了，下次我要先夸再问)".to_string(),
        },
        Message {
            role: "user".to_string(),
            content: "（记住了，下次我要这样）".to_string(),
        },
    ];

    let notes = extract_bracket_self_evolution_notes("jayne-main", &messages);
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].category, "decision");
    assert_eq!(
        notes[0].text,
        "原来他喜欢这种直球，记住了，下次我要先夸再问"
    );
}
#[test]
fn classify_bracket_self_evolution_respects_priority() {
    assert_eq!(
        classify_bracket_self_evolution("原来他不喜欢被追问，这是雷区"),
        "preference"
    );
    assert_eq!(
        classify_bracket_self_evolution("这样更有效，下次我会先顺着他"),
        "decision"
    );
    assert_eq!(
        classify_bracket_self_evolution("这种方式有用，但刚才策略失败了"),
        "experience"
    );
}
#[test]
fn build_bracket_self_evolution_id_is_stable() {
    let first = build_bracket_self_evolution_id("jayne-main", "记住了，下次我要先夸再问");
    let second = build_bracket_self_evolution_id("jayne-main", "记住了，下次我要先夸再问");
    let third = build_bracket_self_evolution_id("other-agent", "记住了，下次我要先夸再问");

    assert_eq!(first, second);
    assert_ne!(first, third);
}
