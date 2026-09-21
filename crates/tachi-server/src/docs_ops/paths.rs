use std::path::{Component, Path};

pub(super) fn canonical_relative_path(path: &Path) -> Result<String, String> {
    let mut components = Vec::new();
    for component in path.components() {
        let Component::Normal(value) = component else {
            return Err(
                "Refusing Wiki organize: protected invariant: document identity contains a non-canonical path component"
                    .to_string(),
            );
        };
        let value = value.to_str().ok_or_else(|| {
            "Refusing Wiki organize: protected invariant: document identity is not UTF-8"
                .to_string()
        })?;
        // Receipt object ids use `/` between filesystem components. A literal
        // backslash is a legal Unix filename byte, so rewriting it would make
        // `a\\b.md` collide with the distinct path `a/b.md`.
        if value.is_empty() || value.contains(['/', '\\']) {
            return Err(
                "Refusing Wiki organize: protected invariant: document identity contains an ambiguous path component"
                    .to_string(),
            );
        }
        components.push(value);
    }
    if components.is_empty() {
        return Err(
            "Refusing Wiki organize: protected invariant: document identity is empty".to_string(),
        );
    }
    Ok(components.join("/"))
}

pub(super) fn canonical_accepted_category(category: &str) -> Result<Option<String>, String> {
    let relative = category.strip_prefix("docs/").unwrap_or(category);
    if relative.is_empty()
        || relative.starts_with('/')
        || relative.ends_with('/')
        || relative.contains('\\')
    {
        return Err(
            "Refusing Wiki organize: protected invariant: category has ambiguous path spelling"
                .to_string(),
        );
    }
    let components = relative.split('/').collect::<Vec<_>>();
    if components
        .iter()
        .any(|component| component.is_empty() || matches!(*component, "." | ".."))
    {
        return Err(
            "Refusing Wiki organize: protected invariant: category has ambiguous path spelling"
                .to_string(),
        );
    }
    let canonical = components.join("/");
    let accepted = ["engineering", "product", "agent"].iter().any(|family| {
        canonical.as_str() == *family
            || canonical
                .strip_prefix(*family)
                .is_some_and(|suffix| suffix.starts_with('/'))
    });
    Ok(accepted.then_some(canonical))
}

pub(super) fn is_archive_dir(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == "archive")
}

pub(super) fn is_markdown_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
}

#[cfg(test)]
mod tests {
    use super::{canonical_accepted_category, canonical_relative_path};
    use std::path::Path;

    #[test]
    fn canonical_document_identity_distinguishes_components_from_literal_backslash() {
        assert_eq!(
            canonical_relative_path(Path::new("docs/a/b.md")).unwrap(),
            "docs/a/b.md"
        );
        #[cfg(unix)]
        assert!(canonical_relative_path(Path::new("docs/a\\b.md"))
            .unwrap_err()
            .contains("ambiguous path component"));
    }

    #[test]
    fn accepted_category_requires_one_canonical_spelling() {
        assert_eq!(
            canonical_accepted_category("docs/engineering/devops").unwrap(),
            Some("engineering/devops".to_string())
        );
        assert_eq!(canonical_accepted_category("legacy").unwrap(), None);
        for alias in [
            "engineering//devops",
            "engineering/./devops",
            "product/acme/",
            "product\\acme",
        ] {
            assert!(
                canonical_accepted_category(alias)
                    .unwrap_err()
                    .contains("ambiguous path spelling"),
                "{alias}"
            );
        }
    }
}
