mod classify;
mod frontmatter;
mod organize;
mod paths;
mod tasks;

pub(crate) use organize::handle_wiki_organize;
#[cfg(test)]
pub(crate) use organize::{set_organize_test_hook, OrganizeTestPoint};
