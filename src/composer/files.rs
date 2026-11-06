use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};

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
    let query = args["query"].as_str().unwrap_or("").to_lowercase();
    let mut files = Vec::new();
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
        let entry = entry?;
        if !entry.file_type().is_some_and(|f| f.is_file()) {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(&root)?
            .to_string_lossy()
            .replace('\\', "/");
        if relative.to_lowercase().contains(&query) {
            files.push(json!({"relative":relative,"path":entry.path().to_string_lossy()}));
            if files.len() >= 100 {
                break;
            }
        }
    }
    files.sort_by(|a, b| a["relative"].as_str().cmp(&b["relative"].as_str()));
    Ok(json!({"root":root,"files":files,"cancelled":false}))
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
    metadata(&path, true)
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
