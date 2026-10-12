//! Pure filesystem path admission used before capability-based file I/O.

use std::path::{Component, Path};

pub fn relative_under<'a>(root: &Path, path: &'a Path) -> Option<&'a Path> {
    if !path.is_absolute() {
        return None;
    }
    let relative = path.strip_prefix(root).ok()?;
    if relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return None;
    }
    Some(relative)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_lexical_escape_and_prefix_confusion() {
        let root = Path::new("/workspace");
        assert_eq!(
            relative_under(root, Path::new("/workspace/src/lib.rs")),
            Some(Path::new("src/lib.rs"))
        );
        assert_eq!(
            relative_under(root, Path::new("/workspace/../secret")),
            None
        );
        assert_eq!(
            relative_under(root, Path::new("/workspace-other/file")),
            None
        );
        assert_eq!(relative_under(root, Path::new("workspace/file")), None);
    }
}
