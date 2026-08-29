use regex::Regex;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use tokio::process::Command;
use zip::ZipArchive;

const MAX_EXTRACTED_TEXT_CHARS: usize = 1_500_000;
const MAX_SCANNED_PDF_PAGES: usize = 6;
const MAX_RENDERED_PDF_BYTES: usize = 12 * 1024 * 1024;
const MAX_ZIP_XML_BYTES: u64 = 8 * 1024 * 1024;
const MAX_PDF_STREAM_BYTES: usize = 8 * 1024 * 1024;
const MAX_XLSX_WORKSHEETS: usize = 64;

#[derive(Debug, Default)]
pub struct ExtractedDocument {
    pub text: Option<String>,
    pub rendered_pages: Vec<Vec<u8>>,
    pub warning: Option<String>,
}

pub fn is_extractable_document(mime: &str, name: &str) -> bool {
    let mime = mime.to_ascii_lowercase();
    let name = name.to_ascii_lowercase();
    mime.starts_with("text/")
        || mime == "application/json"
        || mime == "application/pdf"
        || mime == "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        || mime == "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        || [
            ".txt",
            ".md",
            ".markdown",
            ".json",
            ".csv",
            ".log",
            ".rs",
            ".go",
            ".py",
            ".js",
            ".ts",
            ".tsx",
            ".jsx",
            ".toml",
            ".yaml",
            ".yml",
            ".xml",
            ".html",
            ".css",
            ".sh",
            ".sql",
            ".pdf",
            ".docx",
            ".xlsx",
        ]
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

pub async fn extract_document(
    data: Vec<u8>,
    mime: &str,
    name: &str,
) -> Result<ExtractedDocument, String> {
    let mime = mime.to_ascii_lowercase();
    let name_lower = name.to_ascii_lowercase();

    if mime.starts_with("text/")
        || mime == "application/json"
        || [
            ".txt",
            ".md",
            ".markdown",
            ".json",
            ".csv",
            ".log",
            ".rs",
            ".go",
            ".py",
            ".js",
            ".ts",
            ".tsx",
            ".jsx",
            ".toml",
            ".yaml",
            ".yml",
            ".xml",
            ".html",
            ".css",
            ".sh",
            ".sql",
        ]
        .iter()
        .any(|suffix| name_lower.ends_with(suffix))
    {
        let text = String::from_utf8(data)
            .map_err(|_| "Dokumen teks harus menggunakan encoding UTF-8.".to_string())?;
        return Ok(ExtractedDocument {
            text: Some(limit_text(text)),
            ..Default::default()
        });
    }

    if mime == "application/pdf" || name_lower.ends_with(".pdf") {
        let pdf_bytes = data.clone();
        let extracted = tokio::task::spawn_blocking(move || -> Result<String, String> {
            let document = lopdf::Document::load_mem_with_options(
                &pdf_bytes,
                lopdf::LoadOptions::with_max_decompressed_size(MAX_PDF_STREAM_BYTES),
            )
            .map_err(|err| format!("PDF tidak dapat dibaca: {err}"))?;
            let pages: Vec<u32> = document.get_pages().keys().copied().collect();
            document
                .extract_text_with_limit(&pages, MAX_PDF_STREAM_BYTES)
                .map_err(|err| format!("Teks PDF tidak dapat diekstrak: {err}"))
        })
        .await
        .map_err(|err| format!("Task extractor PDF gagal: {err}"))??;
        let cleaned = normalize_extracted_text(&extracted);
        if cleaned.chars().filter(|c| !c.is_whitespace()).count() >= 24 {
            return Ok(ExtractedDocument {
                text: Some(limit_text(cleaned)),
                ..Default::default()
            });
        }

        match render_scanned_pdf_pages(&data).await {
            Ok(pages) if !pages.is_empty() => Ok(ExtractedDocument {
                text: None,
                rendered_pages: pages,
                warning: Some("PDF tampaknya berbasis gambar; halaman dirender dan akan dianalisis lewat vision model.".to_string()),
            }),
            Ok(_) => Err("PDF tidak memiliki teks yang dapat diekstrak dan renderer tidak menghasilkan halaman.".to_string()),
            Err(err) => Err(format!(
                "PDF tampaknya berupa scan/gambar dan memerlukan OCR/vision. {err}"
            )),
        }
    } else if mime == "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        || name_lower.ends_with(".docx")
    {
        let bytes = data;
        let text = tokio::task::spawn_blocking(move || extract_docx_text(&bytes))
            .await
            .map_err(|err| format!("Task extractor DOCX gagal: {err}"))??;
        Ok(ExtractedDocument {
            text: Some(limit_text(text)),
            ..Default::default()
        })
    } else if mime == "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        || name_lower.ends_with(".xlsx")
    {
        let bytes = data;
        let text = tokio::task::spawn_blocking(move || extract_xlsx_text(&bytes))
            .await
            .map_err(|err| format!("Task extractor XLSX gagal: {err}"))??;
        Ok(ExtractedDocument {
            text: Some(limit_text(text)),
            ..Default::default()
        })
    } else {
        Err("Format dokumen belum didukung extractor Xiao.".to_string())
    }
}

fn extract_docx_text(data: &[u8]) -> Result<String, String> {
    let mut archive =
        ZipArchive::new(Cursor::new(data)).map_err(|err| format!("DOCX invalid: {err}"))?;
    let mut file = archive
        .by_name("word/document.xml")
        .map_err(|err| format!("DOCX tidak memiliki word/document.xml: {err}"))?;
    if file.size() > MAX_ZIP_XML_BYTES {
        return Err("DOCX document.xml terlalu besar untuk diekstrak dengan aman.".to_string());
    }
    let mut xml = String::new();
    file.read_to_string(&mut xml)
        .map_err(|err| format!("Gagal membaca XML DOCX: {err}"))?;

    let paragraph_end = Regex::new(r"(?i)</w:p>").map_err(|err| err.to_string())?;
    let tab = Regex::new(r"(?i)<w:tab\s*/>").map_err(|err| err.to_string())?;
    let breaks = Regex::new(r"(?i)<w:(br|cr)\s*/>").map_err(|err| err.to_string())?;
    let tags = Regex::new(r"(?s)<[^>]+>").map_err(|err| err.to_string())?;
    let xml = paragraph_end.replace_all(&xml, "\n");
    let xml = tab.replace_all(&xml, "\t");
    let xml = breaks.replace_all(&xml, "\n");
    let stripped = tags.replace_all(&xml, "");
    Ok(normalize_extracted_text(
        html_escape::decode_html_entities(&stripped).as_ref(),
    ))
}

fn extract_xlsx_text(data: &[u8]) -> Result<String, String> {
    let mut archive =
        ZipArchive::new(Cursor::new(data)).map_err(|err| format!("XLSX invalid: {err}"))?;
    let shared = read_zip_text_optional(&mut archive, "xl/sharedStrings.xml")?;
    let shared_strings = shared
        .as_deref()
        .map(extract_all_t_nodes)
        .transpose()?
        .unwrap_or_default();

    let mut worksheet_names = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|err| err.to_string())?;
        let name = entry.name().to_string();
        if name.starts_with("xl/worksheets/sheet") && name.ends_with(".xml") {
            worksheet_names.push(name);
            if worksheet_names.len() > MAX_XLSX_WORKSHEETS {
                return Err(format!("XLSX memiliki lebih dari {MAX_XLSX_WORKSHEETS} worksheet; ditolak untuk mencegah resource exhaustion."));
            }
        }
    }
    worksheet_names.sort();

    let cell_re = Regex::new(r#"(?s)<c\b([^>]*)>(.*?)</c>"#).map_err(|err| err.to_string())?;
    let value_re = Regex::new(r"(?s)<v>(.*?)</v>").map_err(|err| err.to_string())?;
    let inline_re = Regex::new(r"(?s)<t[^>]*>(.*?)</t>").map_err(|err| err.to_string())?;
    let mut output = String::new();

    for sheet_name in worksheet_names {
        let xml = read_zip_text(&mut archive, &sheet_name)?;
        if !output.is_empty() {
            output.push_str("\n\n");
        }
        output.push_str(&format!(
            "[{}]\n",
            Path::new(&sheet_name)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("sheet")
        ));
        let mut row_values = Vec::new();
        for captures in cell_re.captures_iter(&xml) {
            let attrs = captures.get(1).map(|m| m.as_str()).unwrap_or("");
            let body = captures.get(2).map(|m| m.as_str()).unwrap_or("");
            let value = if attrs.contains("t=\"s\"") {
                value_re
                    .captures(body)
                    .and_then(|caps| caps.get(1))
                    .and_then(|m| m.as_str().trim().parse::<usize>().ok())
                    .and_then(|idx| shared_strings.get(idx).cloned())
                    .unwrap_or_default()
            } else if attrs.contains("t=\"inlineStr\"") {
                inline_re
                    .captures(body)
                    .and_then(|caps| caps.get(1))
                    .map(|m| html_escape::decode_html_entities(m.as_str()).to_string())
                    .unwrap_or_default()
            } else {
                value_re
                    .captures(body)
                    .and_then(|caps| caps.get(1))
                    .map(|m| m.as_str().trim().to_string())
                    .unwrap_or_default()
            };
            if !value.is_empty() {
                row_values.push(value);
            }
        }
        output.push_str(&row_values.join("\t"));
    }

    let normalized = normalize_extracted_text(&output);
    if normalized.trim().is_empty() {
        Err("XLSX tidak mengandung nilai sel yang dapat diekstrak.".to_string())
    } else {
        Ok(normalized)
    }
}

fn read_zip_text_optional<R: std::io::Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
) -> Result<Option<String>, String> {
    match archive.by_name(name) {
        Ok(mut file) => {
            if file.size() > MAX_ZIP_XML_BYTES {
                return Err(format!(
                    "Entry {name} terlalu besar untuk diekstrak dengan aman."
                ));
            }
            let mut value = String::new();
            file.read_to_string(&mut value)
                .map_err(|err| format!("Gagal membaca {name}: {err}"))?;
            Ok(Some(value))
        }
        Err(zip::result::ZipError::FileNotFound) => Ok(None),
        Err(err) => Err(format!("Gagal membuka {name}: {err}")),
    }
}

fn read_zip_text<R: std::io::Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
) -> Result<String, String> {
    read_zip_text_optional(archive, name)?.ok_or_else(|| format!("{name} tidak ditemukan"))
}

fn extract_all_t_nodes(xml: &str) -> Result<Vec<String>, String> {
    let re = Regex::new(r"(?s)<t[^>]*>(.*?)</t>").map_err(|err| err.to_string())?;
    Ok(re
        .captures_iter(xml)
        .filter_map(|caps| caps.get(1))
        .map(|value| html_escape::decode_html_entities(value.as_str()).to_string())
        .collect())
}

fn normalize_extracted_text(text: &str) -> String {
    let mut output = String::new();
    let mut previous_blank = false;
    for line in text.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            if !previous_blank && !output.is_empty() {
                output.push('\n');
            }
            previous_blank = true;
        } else {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(line);
            previous_blank = false;
        }
    }
    output.trim().to_string()
}

fn limit_text(text: String) -> String {
    if text.chars().count() <= MAX_EXTRACTED_TEXT_CHARS {
        text
    } else {
        let mut limited: String = text.chars().take(MAX_EXTRACTED_TEXT_CHARS).collect();
        limited.push_str("\n\n[Dokumen dipotong oleh batas konteks extractor Xiao]");
        limited
    }
}

async fn render_scanned_pdf_pages(data: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let suffix: u64 = rand::random();
    let base = std::env::temp_dir().join(format!("xiao-pdf-{suffix}"));
    let input = base.with_extension("pdf");
    let prefix = base.with_extension("page");
    tokio::fs::write(&input, data)
        .await
        .map_err(|err| format!("Gagal menulis PDF sementara: {err}"))?;

    let output = Command::new("pdftoppm")
        .arg("-png")
        .arg("-r")
        .arg("120")
        .arg("-f")
        .arg("1")
        .arg("-l")
        .arg(MAX_SCANNED_PDF_PAGES.to_string())
        .arg(&input)
        .arg(&prefix)
        .output()
        .await;

    let _ = tokio::fs::remove_file(&input).await;
    let output = output.map_err(|err| {
        format!("Renderer pdftoppm tidak tersedia ({err}). Instal poppler-utils pada host Linux untuk OCR PDF scan.")
    })?;
    if !output.status.success() {
        return Err(format!(
            "pdftoppm gagal: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let mut pages = Vec::new();
    let mut total = 0usize;
    for index in 1..=MAX_SCANNED_PDF_PAGES {
        let path = rendered_page_path(&prefix, index);
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        let _ = tokio::fs::remove_file(&path).await;
        total = total.saturating_add(bytes.len());
        if total > MAX_RENDERED_PDF_BYTES {
            break;
        }
        pages.push(bytes);
    }
    Ok(pages)
}

fn rendered_page_path(prefix: &Path, index: usize) -> PathBuf {
    PathBuf::from(format!("{}-{index}.png", prefix.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn recognizes_supported_documents() {
        assert!(is_extractable_document("application/pdf", "x.bin"));
        assert!(is_extractable_document("", "notes.docx"));
        assert!(is_extractable_document("text/plain", "file"));
        assert!(!is_extractable_document(
            "application/octet-stream",
            "archive.zip"
        ));
    }

    #[test]
    fn docx_xml_is_extracted() {
        let cursor = Cursor::new(Vec::<u8>::new());
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("word/document.xml", options).unwrap();
        writer
            .write_all(br#"<w:document><w:body><w:p><w:r><w:t>Hello &amp; world</w:t></w:r></w:p><w:p><w:r><w:t>Second</w:t></w:r></w:p></w:body></w:document>"#)
            .unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        let text = extract_docx_text(&bytes).unwrap();
        assert!(text.contains("Hello & world"));
        assert!(text.contains("Second"));
    }
}
