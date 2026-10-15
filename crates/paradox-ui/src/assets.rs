use anyhow::{Result, ensure};
use rust_embed::RustEmbed;
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(RustEmbed)]
#[folder = "../../frontend/dist/"]
struct Embedded;

pub(crate) struct Asset {
    pub bytes: Vec<u8>,
    pub content_type: String,
}

fn mime(path: &str) -> &'static str {
    match Path::new(path).extension().and_then(|value| value.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "application/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("wasm") => "application/wasm",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

pub(crate) fn get(path: &str, assets_dir: &Option<PathBuf>) -> Result<Option<Asset>> {
    let name = if path == "/" { "index.html" } else { path.strip_prefix('/').unwrap_or(path) };
    if name.is_empty() || Path::new(name).components().any(|component| {
        !matches!(component, Component::Normal(value) if !matches!(value.to_str(), Some(".git" | ".paradox" | "node_modules")))
    }) {
        return Ok(None);
    }
    let bytes = if let Some(directory) = assets_dir {
        let base = fs::canonicalize(directory)?;
        ensure!(base.is_dir(), "UI assets must be a directory");
        let mut target = base.clone();
        for component in Path::new(name).components() {
            target.push(component);
            match fs::symlink_metadata(&target) {
                Ok(meta) if meta.file_type().is_symlink() => return Ok(None),
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error.into()),
            }
        }
        let target = fs::canonicalize(target)?;
        if !target.starts_with(&base) || !target.is_file() {
            return Ok(None);
        }
        ensure!(fs::metadata(&target)?.len() <= 32 * 1024 * 1024, "UI asset exceeds 32 MiB");
        fs::read(target)?
    } else {
        let Some(file) = Embedded::get(name) else { return Ok(None) };
        file.data.into_owned()
    };
    Ok(Some(Asset { bytes, content_type: mime(name).into() }))
}

#[cfg(test)]
mod tests {
    use super::get;
    use std::fs;

    #[test]
    fn serves_node_output_and_refuses_traversal_or_repository_metadata() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("index.html"), b"<!doctype html>\n").unwrap();
        fs::create_dir(root.path().join("assets")).unwrap();
        fs::write(root.path().join("assets/editor.js"), b"export {}\n").unwrap();
        let directory = Some(root.path().to_path_buf());
        assert_eq!(get("/", &directory).unwrap().unwrap().bytes, b"<!doctype html>\n");
        assert!(get("/assets/editor.js", &directory).unwrap().unwrap().content_type.starts_with("application/javascript"));
        for path in ["/../index.html", "/assets/../../index.html", "/.git/config", "/.paradox/config.toml", "/missing.js"] {
            assert!(get(path, &directory).unwrap().is_none(), "{path}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn development_assets_never_follow_links() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("private.txt"), b"private").unwrap();
        std::os::unix::fs::symlink(outside.path().join("private.txt"), root.path().join("linked.txt")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("linked-dir")).unwrap();
        let directory = Some(root.path().to_path_buf());
        assert!(get("/linked.txt", &directory).unwrap().is_none());
        assert!(get("/linked-dir/private.txt", &directory).unwrap().is_none());
    }

    #[test]
    fn production_contains_the_built_frontend() {
        let index = get("/", &None).unwrap().unwrap();
        assert!(String::from_utf8(index.bytes).unwrap().contains("<title>Paradox</title>"));
    }
}
