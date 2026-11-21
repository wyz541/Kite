use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use image::{GenericImageView as _, ImageReader};

pub const TEXT_PREVIEW_READ_BYTES: u64 = 64 * 1024;
pub const TEXT_PREVIEW_CHAR_LIMIT: usize = 6_000;
pub const LOG_PREVIEW_READ_BYTES: u64 = 64 * 1024;
pub const IMAGE_PREVIEW_MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
pub const IMAGE_PREVIEW_MAX_SOURCE_EDGE: u32 = 32_768;
pub const IMAGE_PREVIEW_MAX_DECODE_BYTES: u64 = 256 * 1024 * 1024;
const LIVE_MARKDOWN_MAX_BYTES: usize = 256 * 1024;

pub fn markdown_preview_source(source: &str) -> &str {
    if source.len() > LIVE_MARKDOWN_MAX_BYTES {
        "文档超过 256 KiB，已暂停实时预览。仍可编辑和保存。"
    } else if source.trim().is_empty() {
        "*输入 Markdown 后，预览会显示在这里。*"
    } else {
        source
    }
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn render_markdown_preview_html(source: &str) -> String {
    markdown::to_html_with_options(markdown_preview_source(source), &markdown::Options::gfm())
        .unwrap_or_else(|_| "<p>无法渲染 Markdown 预览。</p>".to_string())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewImageData {
    pub width: u32,
    pub height: u32,
    /// Pixels in the BGRA byte order expected by the renderer.
    pub bgra: Vec<u8>,
}

pub fn is_raster_image_preview_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico"
            )
        })
}

/// Reads a bounded text prefix and returns a UTF-8-lossy excerpt.
///
/// The preview never reads more than [`TEXT_PREVIEW_READ_BYTES`] or returns
/// more than [`TEXT_PREVIEW_CHAR_LIMIT`] characters.
pub fn read_text_preview(path: &Path) -> Result<String, String> {
    let file = File::open(path).map_err(|error| format!("无法读取预览：{error}"))?;
    let mut reader = BufReader::new(file).take(TEXT_PREVIEW_READ_BYTES);
    let mut bytes = Vec::with_capacity(TEXT_PREVIEW_READ_BYTES as usize);
    reader
        .read_to_end(&mut bytes)
        .map_err(|error| format!("无法读取预览：{error}"))?;
    if bytes.contains(&0) {
        return Err("此文件不是文本文件".to_string());
    }

    let text = String::from_utf8_lossy(&bytes);
    let mut chars = text.chars();
    let mut excerpt = String::with_capacity(TEXT_PREVIEW_CHAR_LIMIT);
    excerpt.extend(chars.by_ref().take(TEXT_PREVIEW_CHAR_LIMIT));
    if chars.next().is_some() {
        excerpt.push('…');
    }

    Ok(if excerpt.trim().is_empty() {
        "文件没有可显示的文本内容。".to_string()
    } else {
        excerpt
    })
}

/// Reads only the tail of a log file and keeps the final bounded character window.
pub fn read_log_preview(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| format!("无法读取日志预览：{error}"))?;
    let length = file
        .metadata()
        .map_err(|error| format!("无法读取日志信息：{error}"))?
        .len();
    let truncated = length > LOG_PREVIEW_READ_BYTES;
    if truncated {
        file.seek(SeekFrom::Start(length - LOG_PREVIEW_READ_BYTES))
            .map_err(|error| format!("无法定位日志末尾：{error}"))?;
    }

    let mut bytes = Vec::with_capacity(LOG_PREVIEW_READ_BYTES as usize);
    BufReader::new(file)
        .take(LOG_PREVIEW_READ_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("无法读取日志预览：{error}"))?;
    if bytes.contains(&0) {
        return Err("此日志包含二进制内容，无法显示文本预览".to_string());
    }

    let text = String::from_utf8_lossy(&bytes);
    let text = if truncated {
        text.split_once('\n')
            .map_or_else(|| text.as_ref(), |(_, rest)| rest)
    } else {
        text.as_ref()
    };
    let tail_start = text
        .char_indices()
        .rev()
        .nth(TEXT_PREVIEW_CHAR_LIMIT - 1)
        .map_or(0, |(index, _)| index);
    let excerpt = &text[tail_start..];
    if excerpt.trim().is_empty() {
        return Ok("日志没有可显示的文本内容。".to_string());
    }

    if truncated || tail_start > 0 {
        Ok(format!("日志末尾预览：\n{excerpt}"))
    } else {
        Ok(excerpt.to_string())
    }
}

/// Decodes one frame at a bounded size for the preview pane.
///
/// Animated GIF/WebP files intentionally use their first frame. Decoding every
/// frame before the first paint was the dominant cost for large animations.
pub fn load_image_preview(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<PreviewImageData, String> {
    let length = std::fs::metadata(path)
        .map_err(|error| format!("无法读取图片信息：{error}"))?
        .len();
    if length > IMAGE_PREVIEW_MAX_FILE_BYTES {
        return Err(format!(
            "图片超过 {} MB，已跳过预览",
            IMAGE_PREVIEW_MAX_FILE_BYTES / 1024 / 1024
        ));
    }

    let mut reader = ImageReader::open(path)
        .map_err(|error| format!("无法打开图片预览：{error}"))?
        .with_guessed_format()
        .map_err(|error| format!("无法识别图片格式：{error}"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(IMAGE_PREVIEW_MAX_SOURCE_EDGE);
    limits.max_image_height = Some(IMAGE_PREVIEW_MAX_SOURCE_EDGE);
    limits.max_alloc = Some(IMAGE_PREVIEW_MAX_DECODE_BYTES);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|error| format!("无法解码图片预览：{error}"))?;

    let (source_width, source_height) = decoded.dimensions();
    if source_width == 0 || source_height == 0 {
        return Err("图片尺寸无效".to_string());
    }
    let target = fitted_preview_size(source_width, source_height, max_width, max_height);
    let decoded = if target == (source_width, source_height) {
        decoded
    } else {
        decoded.resize_exact(target.0, target.1, image::imageops::FilterType::Triangle)
    };
    let mut rgba = decoded.into_rgba8();
    for pixel in rgba.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Ok(PreviewImageData {
        width: rgba.width(),
        height: rgba.height(),
        bgra: rgba.into_raw(),
    })
}

fn fitted_preview_size(
    source_width: u32,
    source_height: u32,
    max_width: u32,
    max_height: u32,
) -> (u32, u32) {
    let max_width = max_width.max(1);
    let max_height = max_height.max(1);
    if source_width <= max_width && source_height <= max_height {
        return (source_width, source_height);
    }
    let width_scale = max_width as f64 / source_width as f64;
    let height_scale = max_height as f64 / source_height as f64;
    let scale = width_scale.min(height_scale);
    (
        ((source_width as f64 * scale).round() as u32).max(1),
        ((source_height as f64 * scale).round() as u32).max(1),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use image::{Rgba, RgbaImage};

    use super::{fitted_preview_size, load_image_preview, read_log_preview, read_text_preview};

    fn temporary_root(label: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kite-preview-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create preview directory");
        root
    }

    #[test]
    fn text_preview_is_bounded_and_rejects_binary_content() {
        let root = temporary_root("text");
        let text_path = root.join("notes.txt");
        let binary_path = root.join("binary.txt");
        fs::write(&text_path, "first line\nsecond line\n").expect("write text preview");
        fs::write(&binary_path, b"text\0binary").expect("write binary preview");

        let preview = read_text_preview(&text_path).expect("read text preview");
        assert!(preview.contains("first line"));
        assert!(read_text_preview(&binary_path).is_err());
        assert!(
            read_text_preview(&text_path)
                .expect("bounded preview")
                .chars()
                .count()
                <= super::TEXT_PREVIEW_CHAR_LIMIT + 1
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn log_preview_reads_only_the_tail() {
        let root = temporary_root("log");
        let path = root.join("application.log");
        let mut content = String::from("oldest-entry\n");
        content.push_str(&"a".repeat(128 * 1024));
        content.push_str("\nnewest-entry\n");
        fs::write(&path, content).expect("write log preview");

        let preview = read_log_preview(&path).expect("read log preview");
        assert!(preview.starts_with("日志末尾预览："));
        assert!(preview.contains("newest-entry"));
        assert!(!preview.contains("oldest-entry"));
        assert!(preview.chars().count() < super::TEXT_PREVIEW_CHAR_LIMIT + 64);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn image_preview_is_downscaled_and_converted_to_bgra() {
        let root = temporary_root("image");
        let path = root.join("large.png");
        let image = RgbaImage::from_pixel(800, 400, Rgba([10, 20, 30, 255]));
        image.save(&path).expect("write preview image");

        let preview = load_image_preview(&path, 200, 200).expect("load image preview");
        assert_eq!((preview.width, preview.height), (200, 100));
        assert_eq!(preview.bgra.len(), 200 * 100 * 4);
        let first = &preview.bgra[..4];
        assert_eq!(first, &[30, 20, 10, 255]);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn preview_fit_preserves_aspect_ratio_and_never_upscales() {
        assert_eq!(fitted_preview_size(800, 400, 200, 200), (200, 100));
        assert_eq!(fitted_preview_size(400, 800, 200, 200), (100, 200));
        assert_eq!(fitted_preview_size(40, 30, 200, 200), (40, 30));
    }

    #[test]
    fn live_markdown_bounds_input_and_renders_gfm_without_active_html() {
        let source =
            "# 标题\n\n- [x] 完成\n\n```rust\nfn main() {}\n```\n\n<script>alert(1)</script>";
        let rendered = super::render_markdown_preview_html(source);
        assert!(rendered.contains("<h1>标题</h1>"));
        assert!(rendered.contains("type=\"checkbox\""));
        assert!(rendered.contains("class=\"language-rust\""));
        assert!(!rendered.contains("<script>"));
        assert_eq!(super::markdown_preview_source(source), source);
        assert!(super::markdown_preview_source(" \n").contains("输入 Markdown"));
        let large = "文".repeat(super::LIVE_MARKDOWN_MAX_BYTES / 3 + 1);
        assert!(super::markdown_preview_source(&large).contains("已暂停实时预览"));
    }
}
