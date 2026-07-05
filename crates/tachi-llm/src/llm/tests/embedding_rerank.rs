use super::*;

fn embedding_values(seed: f64) -> Vec<f64> {
    (0..1024).map(|idx| seed + idx as f64).collect()
}

#[test]
fn voyage_batch_embeddings_accept_matching_response_indexes() {
    let data = vec![
        json!({"index": 0, "embedding": embedding_values(0.0)}),
        json!({"index": 1, "embedding": embedding_values(1000.0)}),
    ];

    let embeddings =
        parse_voyage_batch_embeddings(&data, 2).expect("matching indexes should parse");

    assert_eq!(embeddings.len(), 2);
    assert_eq!(embeddings[0][0], 0.0);
    assert_eq!(embeddings[1][0], 1000.0);
}

#[test]
fn voyage_batch_embeddings_reject_mismatched_response_index() {
    let data = vec![
        json!({"index": 1, "embedding": embedding_values(1000.0)}),
        json!({"index": 0, "embedding": embedding_values(0.0)}),
    ];

    let err = parse_voyage_batch_embeddings(&data, 2)
        .expect_err("out-of-order response indexes should fail");

    assert!(err.contains("index mismatch"));
}

#[test]
fn rerank_document_filter_preserves_original_indices() {
    let docs = vec![
        "first".to_string(),
        "   ".to_string(),
        "second".to_string(),
        "".to_string(),
    ];

    let (filtered, index_map) = non_empty_rerank_documents(&docs);

    assert_eq!(filtered, vec![&docs[0], &docs[2]]);
    assert_eq!(index_map, vec![0, 2]);
}
