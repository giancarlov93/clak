//! The files in a result: images, audio, and any other binary (a PDF, an archive, a
//! .blend). A terminal doesn't show them and a model doesn't read them in base64: they
//! are written to disk, and their path is printed in their place, which an agent can
//! open with its file-reading tool.

use crate::clock::Local;
use crate::invocation::Invocation;
use crate::render;
use crate::util::{Object, full_path, object_of, str_of};
use base64::Engine as _;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How long files stay in the temporary folder before a new one triggers a cleanup.
const KEEP: Duration = Duration::from_secs(24 * 3600);

/// The folder for files without `-o`: `%TEMP%\clak` (on the Mac `$TMPDIR/clak`), owned by
/// the user and nobody else.
pub fn folder() -> PathBuf {
    std::env::temp_dir().join("clak")
}

/// A result too long for the screen, in the same folder as the files: the path instead
/// of the text. An agent truncates a command's output on its own, without saying so; a
/// file can be read back in pieces.
pub fn spill(text: &str, invocation: &Invocation, extension: &str) -> std::io::Result<PathBuf> {
    let folder = folder();
    clean(&folder);
    std::fs::create_dir_all(&folder)?;
    let what = match &invocation.command {
        None => plain(Some(invocation.program.as_deref().unwrap_or(&invocation.op))),
        Some(command) => format!("{}-{}", plain(invocation.program.as_deref()), plain(Some(command))),
    };
    let path = folder.join(format!("{}-{what}{extension}", Local::now().stamp()));
    std::fs::write(&path, text.as_bytes())?;
    Ok(path)
}

/// Writes the files and returns the text to print: the texts as they are, one line with
/// the path for each file. With `-o` the first file goes there and the others next to it
/// (`vista-2.png`); without it, in `%TEMP%\clak`, with the name the program gave it if
/// it has one.
///
/// `files`, if given, receives a `{path, mime, bytes}` for each file written: for
/// `--json`. The error is the message to give the agent.
pub fn save(blocks: &[&Object], invocation: &Invocation, mut files: Option<&mut Vec<Value>>) -> Result<String, String> {
    let folder = folder();
    let stamp = Local::now().stamp();
    let mut lines: Vec<String> = Vec::new();
    let mut n = 0;
    if invocation.output.is_none() {
        clean(&folder);
    }
    for block in blocks {
        if !render::is_binary(block) {
            lines.push(text_of(block));
            continue;
        }
        let resource = if str_of(block, "type") == Some("resource") { object_of(block, "resource") } else { None };
        let (data, mime, uri) = match resource {
            Some(r) => (str_of(r, "blob").unwrap_or(""), str_of(r, "mimeType"), str_of(r, "uri")),
            None => (str_of(block, "data").unwrap_or(""), str_of(block, "mimeType"), None),
        };
        let what = kind(block, mime);
        let Some(bytes) = decode(data) else {
            lines.push(format!("[{what} unreadable: the program sent invalid data]"));
            continue;
        };
        n += 1;
        let path = match &invocation.output {
            Some(output) if n == 1 => full_path(output),
            Some(output) => numbered(output, n),
            None => folder.join(format!("{stamp}-{n}-{}", name_for(invocation, uri, mime))),
        };
        write(&path, &bytes).map_err(|e| format!("cannot write the file: {e}"))?;
        let shown_mime = mime.filter(|m| !m.is_empty()).map(|m| format!(" {m}")).unwrap_or_default();
        lines.push(format!("[{what}{shown_mime}, {}: {}]", size(bytes.len() as u64), path.display()));
        if let Some(files) = files.as_deref_mut() {
            files.push(json!({"path": path.to_string_lossy(), "mime": mime, "bytes": bytes.len()}));
        }
    }
    if n > 0 {
        lines.push(
            if n == 1 {
                "The file is on disk: to see it, read it from there."
            } else {
                "The files are on disk: to see them, read them from there."
            }
            .to_owned(),
        );
    }
    Ok(lines.join("\n"))
}

fn write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)
}

/// Lenient base64: spaces and newlines in between don't count.
fn decode(data: &str) -> Option<Vec<u8>> {
    let compact: String = data.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    base64::engine::general_purpose::STANDARD.decode(compact).ok()
}

/// A text-only resource is printed, with its address in front.
fn text_of(block: &Object) -> String {
    match object_of(block, "resource").filter(|_| str_of(block, "type") == Some("resource")) {
        Some(r) => format!("[{}]\n{}", str_of(r, "uri").unwrap_or(""), str_of(r, "text").unwrap_or("")),
        None => str_of(block, "text").unwrap_or("").to_owned(),
    }
}

fn kind(block: &Object, mime: Option<&str>) -> &'static str {
    match str_of(block, "type") {
        Some("image") => "image",
        Some("audio") => "audio",
        _ if mime.is_some_and(|m| m.len() >= 6 && m[..6].eq_ignore_ascii_case("image/")) => "image",
        _ => "file",
    }
}

/// The name the program gave (the last part of the uri), or one that says where it comes from.
fn name_for(invocation: &Invocation, uri: Option<&str>, mime: Option<&str>) -> String {
    let given = uri.map(|uri| {
        let bare = uri.split(['?', '#']).next().unwrap_or("");
        let last = bare.trim_end_matches('/').rsplit(['/', '\\', ':']).next().unwrap_or("");
        plain(Some(&unescape(last)))
    });
    if let Some(given) = given.as_deref().filter(|g| !g.is_empty() && g.contains('.')) {
        return given.to_owned();
    }
    let suffix = given.filter(|g| !g.is_empty()).map(|g| format!("-{g}")).unwrap_or_default();
    format!(
        "{}-{}{suffix}{}",
        plain(invocation.program.as_deref()),
        plain(invocation.command.as_deref()),
        extension(mime)
    )
}

/// `%20` and friends, like `Uri.UnescapeDataString`: a sequence that isn't UTF-8 stays as is.
fn unescape(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(byte) = hex {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

/// `vista.png` → `vista-2.png`, in the same folder.
fn numbered(path: &str, n: usize) -> PathBuf {
    let full = full_path(path);
    let stem = full.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let extension = full.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let parent = full.parent().map(Path::to_path_buf).unwrap_or_default();
    parent.join(format!("{stem}-{n}{extension}"))
}

fn extension(mime: Option<&str>) -> &'static str {
    let plain = mime.map(|m| m.split(';').next().unwrap_or("").trim().to_ascii_lowercase());
    match plain.as_deref() {
        Some("image/png") => ".png",
        Some("image/jpeg") => ".jpg",
        Some("image/webp") => ".webp",
        Some("image/gif") => ".gif",
        Some("image/svg+xml") => ".svg",
        Some("image/tiff") => ".tif",
        Some("image/x-exr" | "image/aces") => ".exr",
        Some("audio/wav" | "audio/x-wav") => ".wav",
        Some("audio/mpeg") => ".mp3",
        Some("audio/ogg") => ".ogg",
        Some("audio/flac") => ".flac",
        Some("video/mp4") => ".mp4",
        Some("application/pdf") => ".pdf",
        Some("application/zip") => ".zip",
        Some("application/json") => ".json",
        Some("text/plain") => ".txt",
        Some("text/csv") => ".csv",
        Some("model/gltf-binary") => ".glb",
        Some("model/gltf+json") => ".gltf",
        Some("model/obj") => ".obj",
        Some("model/stl") => ".stl",
        Some("application/x-blender") => ".blend",
        Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document") => ".docx",
        Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet") => ".xlsx",
        Some("application/vnd.openxmlformats-officedocument.presentationml.presentation") => ".pptx",
        _ => ".bin",
    }
}

/// `512 bytes`, `12 KB`, `3.4 MB`.
pub fn size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else if bytes < 1024 * 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        let text = format!("{:.1}", bytes as f64 / (1024.0 * 1024.0));
        format!("{} MB", text.strip_suffix(".0").unwrap_or(&text))
    }
}

/// A name that works as a file name everywhere.
fn plain(name: Option<&str>) -> String {
    let kept: String = name
        .unwrap_or("result")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' })
        .collect();
    kept.trim_matches('.').to_owned()
}

/// Yesterday's files: an agent that captures a view at every step leaves many behind.
fn clean(folder: &Path) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else { continue };
        if !metadata.is_file() {
            continue;
        }
        let old = metadata.modified().ok().and_then(|m| now.duration_since(m).ok()).is_some_and(|age| age > KEEP);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i_file_di_un_risultato_vanno_su_disco_e_al_loro_posto_c_e_il_percorso() {
        let folder = std::env::temp_dir().join(format!("clak-files-{}", clak_wire::json::random_hex(4)));
        std::fs::create_dir_all(&folder).unwrap();
        let content = clak_wire::json::parse_str(
            r#"[{"type":"text","text":"Ecco la vista."},
                {"type":"image","data":"iVBORw0KGgo=","mimeType":"image/png"},
                {"type":"resource","resource":{"uri":"file:///progetti/tavolo.blend","mimeType":"application/x-blender","blob":"QkxFTkRFUg=="}},
                {"type":"resource","resource":{"uri":"memo://nota","text":"testo di una risorsa"}}]"#,
        )
        .unwrap();
        let blocks = render::blocks(Some(&content)).unwrap();
        let output = folder.join("vista.png");
        let invocation = Invocation { output: Some(output.to_string_lossy().into_owned()), ..Invocation::call("blender", "capture_viewport") };
        let mut files = Vec::new();
        let text = save(&blocks, &invocation, Some(&mut files)).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgo=").unwrap());
        let second = folder.join("vista-2.png");
        assert_eq!(std::fs::read(&second).unwrap(), b"BLENDER");
        assert!(text.starts_with("Ecco la vista.\n[image image/png, 8 bytes: "), "{text}");
        assert!(text.contains(&format!("[file application/x-blender, 7 bytes: {}]", second.display())));
        assert!(text.contains("[memo://nota]\ntesto di una risorsa"));
        assert!(text.ends_with("The files are on disk: to see them, read them from there."));
        assert_eq!(files.len(), 2);
        assert_eq!(files[0]["mime"], "image/png");
        assert_eq!(files[0]["bytes"], 8);

        // Without -o: in the temporary folder, with the name the program gave.
        let invocation = Invocation { output: None, ..invocation };
        let text = save(&blocks, &invocation, None).unwrap();
        let blend = text.split('\n').find(|l| l.contains("application/x-blender")).unwrap();
        let path = &blend[blend.find(": ").unwrap() + 2..blend.len() - 1];
        assert!(path.ends_with("tavolo.blend"), "{path}");
        assert!(Path::new(path).starts_with(super::folder()));
        let image = text.split('\n').find(|l| l.contains("image/png")).unwrap();
        assert!(image.ends_with("-1-blender-capture_viewport.png]"), "{image}");
        for line in text.split('\n').filter(|l| l.starts_with("[image") || l.starts_with("[file")) {
            let _ = std::fs::remove_file(&line[line.find(": ").unwrap() + 2..line.len() - 1]);
        }
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn i_dati_sbagliati_si_dicono_e_le_misure_si_leggono() {
        let content = clak_wire::json::parse_str(r#"[{"type":"image","data":"%%%","mimeType":"image/png"}]"#).unwrap();
        let blocks = render::blocks(Some(&content)).unwrap();
        let text = save(&blocks, &Invocation::call("x", "y"), None).unwrap();
        assert_eq!(text, "[image unreadable: the program sent invalid data]");
        assert_eq!(size(512), "512 bytes");
        assert_eq!(size(12 * 1024 + 5), "12 KB");
        assert_eq!(size(3 * 1024 * 1024), "3 MB");
        assert_eq!(size(3 * 1024 * 1024 + 400 * 1024), "3.4 MB");
    }

    #[test]
    fn il_nome_viene_dall_uri_o_dal_comando() {
        let invocation = Invocation::call("revit", "export");
        assert_eq!(name_for(&invocation, Some("file:///a/b/pianta%20uno.pdf?x=1"), None), "pianta_uno.pdf");
        assert_eq!(name_for(&invocation, Some("memo://vista/"), Some("image/png")), "revit-export-vista.png");
        assert_eq!(name_for(&invocation, None, Some("application/pdf; charset=x")), "revit-export.pdf");
        assert_eq!(name_for(&invocation, None, None), "revit-export.bin");
    }
}
