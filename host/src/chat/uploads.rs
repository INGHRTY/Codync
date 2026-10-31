//! Files sent with a message. A client uploads each file in chunks (the channel caps a
//! message at 1 MiB), then names the uploads in `send`; the agent gets their paths.

use crate::agent::workspace;
use crate::service;
use crate::store::BotConfig;
use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Largest file accepted.
pub const MAX_FILE: u64 = 100 * 1024 * 1024;

/// Inside a personal workspace, so reading an upload needs no extra approval.
/// A group (no workspace) keeps its members' shared files in its data folder.
pub fn root(cfg: &BotConfig) -> PathBuf {
    if workspace::is_managed(cfg) {
        Path::new(&cfg.cwd).join("uploads")
    } else {
        service::data_dir().join("bots").join(&cfg.id).join("uploads")
    }
}

/// `uploads/<upload id>/`, the id checked so it can't leave the folder.
fn dir(root: &Path, upload: &str) -> Result<PathBuf> {
    uuid::Uuid::parse_str(upload).context("invalid upload id")?;
    Ok(root.join(upload.to_ascii_lowercase()))
}

/// The file's own name only: no folders, no hidden or empty names.
fn clean_name(name: &str) -> Result<String> {
    let name = name.rsplit(['/', '\\']).next().unwrap_or_default().trim();
    if name.is_empty() || name.starts_with('.') || name.len() > 200 {
        bail!("invalid file name");
    }
    Ok(name.to_owned())
}

/// Appends one chunk at `offset`. A repeated chunk (a retry) is accepted as is.
/// Returns the attachment once `done`.
pub fn append(root: &Path, upload: &str, name: &str, offset: u64, data: &[u8], done: bool) -> Result<Option<Value>> {
    let dir = dir(root, upload)?;
    let path = dir.join(clean_name(name)?);
    std::fs::create_dir_all(&dir).context("creating the upload folder")?;
    let len = std::fs::metadata(&path).map_or(0, |m| m.len());
    let end = offset + data.len() as u64;
    if end > MAX_FILE {
        bail!("file is larger than {} MB", MAX_FILE / 1024 / 1024);
    }
    if offset == len {
        let mut file =
            std::fs::OpenOptions::new().create(true).append(true).open(&path).context("opening the upload")?;
        file.write_all(data).context("writing the upload")?;
    } else if end != len {
        bail!("upload out of order: at {len}, got {offset}");
    }
    Ok(done.then(|| json!({"id": upload, "name": path.file_name().map(|n| n.to_string_lossy()), "size": end})))
}

/// A finished upload: its path and chat metadata.
pub fn resolve(root: &Path, upload: &str) -> Result<(PathBuf, Value)> {
    let dir = dir(root, upload)?;
    let file = std::fs::read_dir(&dir)
        .context("unknown upload")?
        .filter_map(Result::ok)
        .find(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .context("empty upload")?;
    let size = file.metadata().map_or(0, |m| m.len());
    let name = file.file_name().to_string_lossy().into_owned();
    Ok((file.path(), json!({"id": upload, "name": name, "size": size})))
}

/// Where a sent attachment (`{id, name}` chat metadata) lives, without touching the disk.
pub fn path_of(root: &Path, meta: &Value) -> Option<PathBuf> {
    let name = clean_name(meta["name"].as_str()?).ok()?;
    Some(dir(root, meta["id"].as_str()?).ok()?.join(name))
}

/// Up to `len` bytes of a finished upload from `offset`, and its full size.
pub fn read(root: &Path, upload: &str, offset: u64, len: usize) -> Result<(Vec<u8>, u64)> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let (path, _) = resolve(root, upload)?;
    let mut file = std::fs::File::open(&path).context("opening the upload")?;
    let size = file.metadata().context("reading the upload")?.len();
    file.seek(SeekFrom::Start(offset)).context("reading the upload")?;
    let mut data = Vec::with_capacity(len);
    file.take(len as u64).read_to_end(&mut data).context("reading the upload")?;
    Ok((data, size))
}

/// What the agent reads after the message text.
pub fn prompt_suffix(paths: &[PathBuf]) -> String {
    let list: Vec<String> = paths.iter().map(|p| format!("- {}", p.display())).collect();
    format!("\n\n[Attached files — read them from these paths]\n{}", list.join("\n"))
}

/// The files a bot on another computer's gateway was sent: it can't open them, so it is told what they are.
pub fn remote_suffix(names: &[String]) -> String {
    let names: Vec<&str> = names.iter().map(String::as_str).filter(|n| !n.is_empty()).collect();
    if names.is_empty() {
        return String::new();
    }
    format!(
        "\n\n[Attached files: {}. They are on the user's computer, which you can't reach — say so if you need to \
         read them]",
        names.join(", ")
    )
}

/// The file types a gateway's Hermes takes as images in the turn itself (`image_url` parts; it refuses
/// file parts).
const IMAGE_TYPES: [(&str, &str); 5] =
    [("png", "image/png"), ("jpg", "image/jpeg"), ("jpeg", "image/jpeg"), ("gif", "image/gif"), ("webp", "image/webp")];

/// How much image data one turn carries: base64 is a third larger, and Hermes caps a request at 10 MB.
const IMAGES_MAX: u64 = 6 * 1024 * 1024;

/// The type an upload's own name gives it, when a gateway can see it as an image.
fn image_type(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    IMAGE_TYPES.iter().find(|(e, _)| *e == ext).map(|(_, mime)| *mime)
}

/// Splits a message's attachments for a bot on another computer's gateway: the images it gets in the turn
/// itself (up to [`IMAGES_MAX`] in all, which are not named again), and the names of the rest, which sit on
/// this computer and can only be described.
pub fn remote_plan(root: &Path, attachments: &[Value]) -> (Vec<PathBuf>, Vec<String>) {
    let mut images = Vec::new();
    let mut named = Vec::new();
    let mut total = 0u64;
    for meta in attachments {
        let name = meta["name"].as_str().unwrap_or_default();
        let path = path_of(root, meta);
        // Only a file that's really there travels: a missing or empty one is named like any other.
        let size = path.as_ref().and_then(|p| std::fs::metadata(p).ok()).map_or(0, |m| m.len());
        match path.filter(|p| size > 0 && image_type(p).is_some() && total + size <= IMAGES_MAX) {
            Some(path) => {
                total += size;
                images.push(path);
            }
            None if !name.is_empty() => named.push(name.to_owned()),
            None => {}
        }
    }
    (images, named)
}

/// Those images as ACP content blocks (`{type, mimeType, data}`), what the actor's prompt carries and a
/// gateway's request turns into `image_url` data URLs, and the names of the images that can't be read.
pub fn image_blocks(paths: &[PathBuf]) -> (Vec<Value>, Vec<String>) {
    use base64::Engine as _;
    let mut blocks = Vec::new();
    let mut unread = Vec::new();
    for path in paths {
        let block = image_type(path).zip(std::fs::read(path).ok()).map(|(mime, data)| {
            json!({"type": "image", "mimeType": mime, "data": base64::engine::general_purpose::STANDARD.encode(data)})
        });
        match block {
            Some(block) => blocks.push(block),
            None => unread.extend(path.file_name().map(|n| n.to_string_lossy().into_owned())),
        }
    }
    (blocks, unread)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_stay_inside_the_folder() {
        assert_eq!(clean_name("../../etc/passwd").expect("last component"), "passwd");
        assert_eq!(clean_name("C:\\x\\photo.jpg").expect("last component"), "photo.jpg");
        assert!(clean_name("..").is_err());
        assert!(clean_name(".ssh").is_err());
        assert!(clean_name("dir/").is_err());
    }

    #[test]
    fn a_gateway_gets_the_images_and_hears_the_other_files() {
        use base64::Engine as _;
        let root = std::env::temp_dir().join(format!("codync-uploads-{}", uuid::Uuid::new_v4()));
        let root = root.as_path();
        let sent = |name: &str, data: &[u8]| {
            let id = uuid::Uuid::new_v4().to_string();
            append(root, &id, name, 0, data, true).expect("upload").expect("done")
        };
        let photo = sent("Photo.PNG", b"\x89PNG");
        let notes = sent("notes.pdf", b"%PDF");
        // A file whose upload is gone is named too: the agent should hear the message had one.
        let lost = json!({"id": uuid::Uuid::new_v4().to_string(), "name": "gone.jpg"});
        let (images, named) = remote_plan(root, &[photo, notes, lost]);
        assert_eq!(named, ["notes.pdf", "gone.jpg"], "no image, or nothing on disk: named instead");
        let data = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG");
        let (blocks, unread) = image_blocks(&images);
        assert_eq!(blocks, [json!({"type": "image", "mimeType": "image/png", "data": data})]);
        assert_eq!(unread, [] as [String; 0]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_image_past_the_turn_budget_is_named_instead_of_sent() {
        let root = std::env::temp_dir().join(format!("codync-uploads-{}", uuid::Uuid::new_v4()));
        let root = root.as_path();
        let sent = |name: &str, data: &[u8]| {
            let id = uuid::Uuid::new_v4().to_string();
            append(root, &id, name, 0, data, true).expect("upload").expect("done")
        };
        // Each image fits a turn on its own; together they don't.
        let half = vec![0u8; usize::try_from(IMAGES_MAX / 2 + 1).expect("image budget fits memory")];
        let first = sent("one.png", &half);
        let second = sent("two.png", &half);
        let (images, named) = remote_plan(root, &[first, second]);
        assert_eq!(images.len(), 1, "the turn carries what fits");
        assert_eq!(named, ["two.png"], "the one over the budget is named, not dropped");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_image_that_cannot_be_read_is_named() {
        let root = std::env::temp_dir().join(format!("codync-uploads-{}", uuid::Uuid::new_v4()));
        let name = root.join("gone.png");
        let (blocks, unread) = image_blocks(&[name]);
        assert_eq!(blocks, [] as [Value; 0]);
        assert_eq!(unread, ["gone.png"]);
    }

    #[test]
    fn chunks_append_and_retries_are_idempotent() {
        let root = std::env::temp_dir().join(format!("codync-uploads-{}", uuid::Uuid::new_v4()));
        let root = root.as_path();
        let id = uuid::Uuid::new_v4().to_string();
        assert!(dir(root, "../x").is_err());
        assert!(append(root, &id, "a.txt", 0, b"hello ", false).expect("first").is_none());
        assert!(append(root, &id, "a.txt", 0, b"hello ", false).is_ok(), "retry of the first chunk");
        assert!(append(root, &id, "a.txt", 3, b"xx", false).is_err(), "out of order");
        let done = append(root, &id, "a.txt", 6, b"world", true).expect("last").expect("done");
        assert_eq!(done["size"], 11);
        let (path, meta) = resolve(root, &id).expect("resolve");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "hello world");
        assert_eq!(meta["name"], "a.txt");
        assert_eq!(read(root, &id, 6, 100).expect("read"), (b"world".to_vec(), 11));
        assert!(path.starts_with(root));
        assert_eq!(path_of(root, &done), Some(path));
        let _ = std::fs::remove_dir_all(root);
    }
}
