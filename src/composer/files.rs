use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};

// Prefer filenames, then paths; fuzzy matching uses Unicode characters, not bytes.
fn rank(relative: &str, query: &str) -> Option<(u8, usize)> {
    let path = relative.to_lowercase();
    let name = path.rsplit('/').next().unwrap_or(&path);
    if query.is_empty() {
        return Some((6, path.len()));
    }
    if name == query {
        return Some((0, 0));
    }
    if name.starts_with(query) {
        return Some((1, name.chars().count()));
    }
    if let Some(at) = name.find(query) {
        return Some((2, at));
    }
    if path.starts_with(query) {
        return Some((3, path.len()));
    }
    if let Some(at) = path.find(query) {
        return Some((4, at));
    }
    let mut positions = path.chars().enumerate();
    let mut first = None;
    let mut last = 0;
    for wanted in query.chars().filter(|c| !c.is_whitespace()) {
        let (at, _) = positions.find(|(_, c)| *c == wanted)?;
        first.get_or_insert(at);
        last = at;
    }
    Some((5, last - first.unwrap_or(0) + path.chars().count()))
}

pub fn project(cwd: &str) -> String {
    crate::spawn::command("git")
        .args(["-C", cwd, "rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| cwd.to_owned())
}

pub fn search(args: &Value, cancelled: impl Fn() -> bool) -> Result<Value> {
    let cwd = args["cwd"]
        .as_str()
        .ok_or_else(|| anyhow!("working directory unavailable"))?;
    let root = PathBuf::from(project(cwd));
    if !root.is_dir() {
        bail!("Working directory does not exist")
    }
    let query = args["query"]
        .as_str()
        .unwrap_or("")
        .replace('\\', "/")
        .to_lowercase();
    let mut files = std::collections::BinaryHeap::new();
    let mut matches = 0;
    let mut errors = 0;
    let walker = ignore::WalkBuilder::new(&root)
        .hidden(false)
        .follow_links(false)
        .require_git(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build();
    for entry in walker {
        if cancelled() {
            return Ok(json!({"cancelled":true,"files":[]}));
        }
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                errors += 1;
                continue;
            }
        };
        if !entry.file_type().is_some_and(|f| f.is_file()) {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(&root)?
            .to_string_lossy()
            .replace('\\', "/");
        if let Some(score) = rank(&relative, &query) {
            matches += 1;
            files.push((score, relative, entry.path().to_owned()));
            if files.len() > 100 {
                files.pop();
            }
        }
    }
    let files: Vec<_> = files
        .into_sorted_vec()
        .into_iter()
        .map(|(_, relative, path)| json!({"relative":relative,"path":path}))
        .collect();
    Ok(json!({"root":root,"files":files,"cancelled":false,"matches":matches,"errors":errors}))
}

pub fn metadata(path: &Path, managed: bool) -> Result<Value> {
    let path = path.canonicalize()?;
    let path = native_path(&path);
    let stat = std::fs::metadata(&path)?;
    if !stat.is_file() {
        bail!("Select a regular file")
    }
    let dimensions = image::ImageReader::open(&path)
        .ok()
        .and_then(|r| r.with_guessed_format().ok())
        .and_then(|r| r.into_dimensions().ok());
    Ok(
        json!({"path":path,"name":path.file_name().map(|s|s.to_string_lossy()),"bytes":stat.len(),
        "width":dimensions.map(|d|d.0),"height":dimensions.map(|d|d.1),"image":dimensions.is_some(),"managed":managed}),
    )
}

/// Decode previews off the UI thread, with the same bounds as clipboard images.
fn thumbnail(path: &Path, folder: &Path) -> Result<PathBuf> {
    use std::hash::{Hash, Hasher};
    let stat = std::fs::metadata(path)?;
    if stat.len() > 32 * 1024 * 1024 {
        bail!("Image preview exceeds 32 MiB")
    }
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hash);
    stat.len().hash(&mut hash);
    stat.modified()?.hash(&mut hash);
    let target = folder.join(format!("{:016x}.png", hash.finish()));
    if target.is_file() {
        return Ok(target);
    }
    let mut reader = image::ImageReader::open(path)?.with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode()?.thumbnail(160, 160);
    std::fs::create_dir_all(folder)?;
    let temp = folder.join(format!("{}.png", uuid::Uuid::new_v4()));
    image.save_with_format(&temp, image::ImageFormat::Png)?;
    match std::fs::rename(&temp, &target) {
        Ok(()) => {}
        Err(_) if target.is_file() => {
            let _ = std::fs::remove_file(&temp);
        }
        Err(e) => return Err(e.into()),
    }
    Ok(target)
}
pub fn reference(path: &Path, managed: bool) -> Result<Value> {
    let mut info = metadata(path, managed)?;
    if info["image"] == true {
        let home = crate::mods::app_dir();
        match thumbnail(
            Path::new(info["path"].as_str().unwrap()),
            &home.join("composer/previews"),
        ) {
            Ok(path) => {
                info["preview"] = json!(
                    path.strip_prefix(home)?
                        .to_string_lossy()
                        .replace('\\', "/")
                )
            }
            Err(e) => info["previewError"] = json!(e.to_string()),
        }
    }
    Ok(info)
}

pub fn clipboard(bytes: Vec<u8>) -> Result<Value> {
    if bytes.len() > 32 * 1024 * 1024 {
        bail!("Clipboard image exceeds 32 MiB")
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode()?;
    let folder = super::store::attachment_folder()?;
    std::fs::create_dir_all(&folder)?;
    let path = folder.join(format!("{}.png", uuid::Uuid::new_v4()));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    let mut writer = std::io::BufWriter::new(file);
    image.write_to(&mut writer, image::ImageFormat::Png)?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    reference(&path, true)
}

pub fn save_clipboard(_args: &Value) -> Result<Value> {
    bail!("Use the explicit native clipboard image action")
}

fn native_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{unc}"))
    } else {
        PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fuzzy_search_ranks_names_before_paths_and_does_not_stop_at_first_hundred() {
        assert!(rank("a/中文 空白.txt", "中文").unwrap() < rank("中文/a.txt", "中文").unwrap());
        assert!(rank("src/long/path/composer.rs", "cmp rs").is_some());
        assert!(rank("src/hello.rs", "zzz").is_none());
        let root = std::env::temp_dir().join(format!("my-herdr-rank-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("image")).unwrap();
        for n in 0..120 {
            std::fs::write(root.join(format!("image/{n}.txt")), "").unwrap();
        }
        std::fs::write(root.join("image.png"), "").unwrap();
        let result = search(&json!({"cwd":root,"query":"image"}), || false).unwrap();
        assert_eq!(result["files"][0]["relative"], "image.png");
        assert_eq!(result["matches"], 121);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn search_respects_ignore_unicode_paths_and_cancellation_outside_git() {
        let root = std::env::temp_dir().join(format!("my-herdr-files-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("ignored")).unwrap();
        std::fs::write(root.join(".gitignore"), "ignored/\n").unwrap();
        std::fs::write(root.join("中文 空白.txt"), "content").unwrap();
        std::fs::write(root.join("ignored/hidden.txt"), "hidden").unwrap();
        std::fs::write(root.join(".git/internal"), "internal").unwrap();
        let args = json!({"cwd":root,"query":""});
        let found = search(&args, || false).unwrap();
        let paths: Vec<_> = found["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["relative"].as_str().unwrap())
            .collect();
        assert_eq!(paths, vec![".gitignore", "中文 空白.txt"]);
        assert_eq!(search(&args, || true).unwrap()["cancelled"], true);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn thumbnails_are_bounded_and_do_not_change_originals() {
        let root = std::env::temp_dir().join(format!("my-herdr-preview-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let original = root.join("中文 圖片.png");
        image::RgbImage::new(800, 400).save(&original).unwrap();
        let before = std::fs::read(&original).unwrap();
        let preview = thumbnail(&original, &root.join("previews")).unwrap();
        assert_eq!(image::image_dimensions(&preview).unwrap(), (160, 80));
        assert_eq!(
            thumbnail(&original, &root.join("previews")).unwrap(),
            preview
        );
        assert_eq!(std::fs::read(&original).unwrap(), before);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn windows_paths_preserve_unc_and_spaces() {
        assert_eq!(
            native_path(Path::new(r"\\?\UNC\server\share\中文 圖片.png")),
            PathBuf::from(r"\\server\share\中文 圖片.png")
        );
        assert_eq!(
            native_path(Path::new(r"\\?\D:\中文 圖片.png")),
            PathBuf::from(r"D:\中文 圖片.png")
        );
    }
    #[test]
    fn references_report_jpeg_dimensions_and_never_modify_originals() {
        let root = std::env::temp_dir().join(format!("my-herdr-image-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("中文 圖片.jpg");
        image::RgbImage::new(3, 5).save(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let info = metadata(&path, false).unwrap();
        assert_eq!(info["image"], true);
        assert_eq!(info["width"], 3);
        assert_eq!(info["height"], 5);
        assert_eq!(info["managed"], false);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::remove_dir_all(root).unwrap();
    }
}
