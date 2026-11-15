#![cfg(unix)]

mod common;

#[test]
fn packaged_score_display_http_dogfood() {
    common::assert_dogfood_scenario("search_markdown_labels_scores_and_documents_the_order");
}
