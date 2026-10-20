use std::collections::HashSet;
use std::io::{Cursor, Read};

use serde::Serialize;
use sha2::{Digest, Sha256};

pub const MAX_ZIP_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_ARCHIVE_ENTRIES: usize = 1000;
pub const MAX_INVOICES: usize = 500;
pub const MAX_EXPANDED_BYTES: u64 = 50 * 1024 * 1024;
pub const MAX_COMPRESSION_RATIO: u64 = 200;

#[derive(Debug, Serialize)]
pub struct InvoiceSource {
    pub original_name: String,
    pub sha256: String,
    pub bytes: usize,
    #[serde(skip)]
    pub pdf: Vec<u8>,
}

#[derive(Debug, thiserror::Error, Serialize)]
#[error("{message}")]
pub struct InvoiceZipError {
    pub member: Option<usize>,
    pub message: &'static str,
}

fn error(member: Option<usize>, message: &'static str) -> InvoiceZipError {
    InvoiceZipError { member, message }
}

pub fn inspect_invoice_zip(bytes: &[u8]) -> Result<Vec<InvoiceSource>, InvoiceZipError> {
    if bytes.len() > MAX_ZIP_BYTES {
        return Err(error(None, "invoice ZIP exceeds 5 MiB"));
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|_| error(None, "invalid or unsupported ZIP archive"))?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(error(None, "invoice ZIP has too many members"));
    }
    let mut names = HashSet::new();
    let mut hashes = HashSet::new();
    let mut total = 0_u64;
    let mut invoices = Vec::new();
    for index in 0..archive.len() {
        let member = Some(index + 1);
        let mut file = archive
            .by_index(index)
            .map_err(|_| error(member, "unreadable or encrypted ZIP member"))?;
        let name = std::str::from_utf8(file.name_raw())
            .map_err(|_| error(member, "ZIP member name must be UTF-8"))?
            .to_owned();
        if name.len() > 240
            || name.contains(['\\', ':'])
            || name.chars().any(char::is_control)
            || file.enclosed_name().is_none()
            || name
                .trim_end_matches('/')
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(error(member, "unsafe ZIP member path"));
        }
        if !names.insert(name.trim_end_matches('/').to_lowercase()) {
            return Err(error(member, "duplicate ZIP member path"));
        }
        let mode = file.unix_mode().unwrap_or(0) & 0o170000;
        if file.encrypted() || file.is_symlink() || !matches!(mode, 0 | 0o100000 | 0o040000) {
            return Err(error(
                member,
                "encrypted or non-regular ZIP members are not supported",
            ));
        }
        if file.is_dir() {
            if file.size() != 0 {
                return Err(error(member, "ZIP directories must be empty"));
            }
            continue;
        }
        if mode == 0o040000 || !name.to_ascii_lowercase().ends_with(".pdf") {
            return Err(error(
                member,
                "invoice ZIP may contain only PDF files and directories",
            ));
        }
        if invoices.len() == MAX_INVOICES {
            return Err(error(member, "invoice ZIP exceeds 500 PDFs"));
        }
        let declared = file.size();
        let compressed = file.compressed_size();
        if declared > crate::store::MAX_PDF_BYTES
            || total.saturating_add(declared) > MAX_EXPANDED_BYTES
            || declared > compressed.saturating_mul(MAX_COMPRESSION_RATIO)
        {
            return Err(error(
                member,
                "ZIP member exceeds expanded size or compression limits",
            ));
        }
        let mut pdf = Vec::new();
        let limit = crate::store::MAX_PDF_BYTES.min(MAX_EXPANDED_BYTES.saturating_sub(total));
        (&mut file)
            .take(limit + 1)
            .read_to_end(&mut pdf)
            .map_err(|_| {
                error(
                    member,
                    "ZIP member failed decompression or CRC verification",
                )
            })?;
        let actual = pdf.len() as u64;
        if actual != declared
            || actual > limit
            || actual > compressed.saturating_mul(MAX_COMPRESSION_RATIO)
        {
            return Err(error(member, "ZIP member actual size exceeds its limits"));
        }
        if !pdf.starts_with(b"%PDF-")
            || !pdf[pdf.len().saturating_sub(1024)..]
                .windows(5)
                .any(|marker| marker == b"%%EOF")
        {
            return Err(error(member, "ZIP member is not a complete PDF document"));
        }
        let sha256 = crate::hex_lower(&Sha256::digest(&pdf));
        if !hashes.insert(sha256.clone()) {
            return Err(error(member, "duplicate PDF contents require review"));
        }
        total += actual;
        invoices.push(InvoiceSource {
            original_name: name,
            sha256,
            bytes: pdf.len(),
            pdf,
        });
    }
    if invoices.is_empty() {
        return Err(error(None, "invoice ZIP contains no PDFs"));
    }
    Ok(invoices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in files {
            writer
                .start_file(
                    *name,
                    SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
                )
                .unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    const PDF: &[u8] = b"%PDF-1.7\noriginal invoice\n%%EOF\n";

    #[test]
    fn preserves_original_bytes_and_exposes_only_metadata_on_serialization() {
        let zip = archive(&[("invoices/original.pdf", PDF)]);
        let result = inspect_invoice_zip(&zip).unwrap();
        assert_eq!(result[0].pdf, PDF);
        assert_eq!(result[0].sha256, crate::hex_lower(&Sha256::digest(PDF)));
        assert!(
            serde_json::to_value(&result[0])
                .unwrap()
                .get("pdf")
                .is_none()
        );
    }

    #[test]
    fn rejects_unsafe_paths_non_pdf_files_and_duplicates() {
        for name in [
            "../invoice.pdf",
            "/invoice.pdf",
            "C:/invoice.pdf",
            "a\\invoice.pdf",
            "a/../invoice.pdf",
            "a//invoice.pdf",
            "nested.zip",
        ] {
            assert!(
                inspect_invoice_zip(&archive(&[(name, PDF)])).is_err(),
                "{name}"
            );
        }
        assert!(inspect_invoice_zip(&archive(&[("a.pdf", PDF), ("b.pdf", PDF)])).is_err());
        assert!(
            inspect_invoice_zip(&archive(&[
                ("a.pdf", PDF),
                ("A.pdf", b"%PDF-1.7\nother\n%%EOF")
            ]))
            .is_err()
        );
    }

    #[test]
    fn rejects_invalid_zip_invalid_pdf_and_oversized_uploads() {
        assert!(inspect_invoice_zip(b"not a zip").is_err());
        assert!(inspect_invoice_zip(&archive(&[])).is_err());
        assert!(inspect_invoice_zip(&archive(&[("a.pdf", b"not a PDF")])).is_err());
        assert!(inspect_invoice_zip(&vec![0; MAX_ZIP_BYTES + 1]).is_err());
    }

    #[test]
    fn rejects_member_exceeding_expanded_limit_before_reading() {
        let data = vec![b' '; crate::store::MAX_PDF_BYTES as usize + 1];
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                "large.pdf",
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
            )
            .unwrap();
        writer.write_all(&data).unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        assert!(bytes.len() < MAX_ZIP_BYTES);
        assert!(
            inspect_invoice_zip(&bytes)
                .unwrap_err()
                .message
                .contains("limits")
        );
    }

    #[test]
    #[ignore = "requires a private archive supplied through HARVEST_INVOICE_ZIP"]
    fn private_invoice_archive_is_inspected_without_extraction() {
        let path = std::env::var("HARVEST_INVOICE_ZIP").expect("set HARVEST_INVOICE_ZIP");
        let bytes = std::fs::read(path).unwrap();
        let invoices = inspect_invoice_zip(&bytes).unwrap();
        eprintln!(
            "invoice ZIP: {} PDFs, {} expanded bytes",
            invoices.len(),
            invoices.iter().map(|invoice| invoice.bytes).sum::<usize>()
        );
        assert!(!invoices.is_empty());
    }
}
