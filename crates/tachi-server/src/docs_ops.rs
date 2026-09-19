mod classify;
mod frontmatter;
mod organize;
mod paths;
mod tasks;

#[cfg(test)]
pub(crate) use classify::enable_model_classification_for_test;
pub(crate) use organize::handle_wiki_organize;
#[cfg(test)]
pub(crate) use organize::{set_new_file_test_hook, set_organize_test_hook, OrganizeTestPoint};
