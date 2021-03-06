use super::DocumentId;
use serde_json::json;
use std::io::Write;
use zip::{result::ZipError, write::FileOptions, ZipWriter};

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("Can't build the ZIP archive: {0}")]
    Zip(#[from] ZipError),

    #[error("Can't serialize json file: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Can't write file into the archive: {0}")]
    IO(#[from] std::io::Error),
}

pub(crate) fn make(
    id: &DocumentId,
    ext: &String,
    content: &Vec<u8>,
) -> Result<Vec<u8>, ArchiveError> {
    let mut buffer: Vec<u8> = Vec::new();
    let w = std::io::Cursor::new(&mut buffer);
    let mut zip = ZipWriter::new(w);

    let options = FileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .unix_permissions(0o644);

    // actual file
    zip.start_file(format!("{}.{}", id.0, ext), options.clone())?;
    zip.write_all(&content)?;

    // .pagedata file
    zip.start_file(format!("{}.pagedata", id.0), options.clone())?;
    zip.write_all(&Vec::new())?;

    // .content file
    zip.start_file(format!("{}.content", id.0), options.clone())?;

    let content = json!(
        {
            "dummyDocument": false,
            "extraMetadata": {
                "LastBrushColor": "",
                "LastBrushThicknessScale": "",
                "LastColor": "",
                "LastEraserThicknessScale": "",
                "LastEraserTool": "",
                "LastPen": "Finelinerv2",
                "LastPenColor": "",
                "LastPenThicknessScale": "",
                "LastPencil": "",
                "LastPencilColor": "",
                "LastPencilThicknessScale": "",
                "LastTool": "Finelinerv2",
                "ThicknessScale": "",
                "LastFinelinerv2Size": "1"
            },
            "fileType": ext,
            "fontName": "EB Garamond",
            "lastOpenedPage": 0,
            "lineHeight": 100,
            "margins": 50,
            "orientation": "portrait",
            "pageCount": 0,
            "pages": null,
            "textAlignment": "justify",
            "textScale": 1.2,
            "transform": {
                "m11": 1,
                "m12": 0,
                "m13": 0,
                "m21": 0,
                "m22": 1,
                "m23": 0,
                "m31": 0,
                "m32": 0,
                "m33": 1
            }
        }
    );
    let content = serde_json::to_vec(&content)?;
    zip.write_all(&content)?;

    // Finalize the archive and drop the borrow on the byte buffer
    zip.finish()?;
    drop(zip);

    Ok(buffer)
}
