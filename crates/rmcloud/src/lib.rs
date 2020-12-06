use chrono::{SecondsFormat, Utc};
use hyper::StatusCode;
use log::debug;
use serde::Deserialize;
use serde_json::json;
use std::path::Path;
use tokio::prelude::*;
use uuid::Uuid;

mod archive;

const DOCUMENT_LIST_URL: &str = "https://document-storage-production-dot-remarkable-production.appspot.com/document-storage/json/2/docs";
const DOCUMENT_UPLOAD_URL: &str = "https://document-storage-production-dot-remarkable-production.appspot.com/document-storage/json/2/upload/request";
const DOCUMENT_UPDATE_URL: &str = "https://document-storage-production-dot-remarkable-production.appspot.com/document-storage/json/2/upload/update-status";

#[derive(Debug)]
pub enum Error {
    NoTokenAvailable,
    NoValidExtensionForUpload(Option<String>),
    NoValidFileNameForUpload,
    FileNameIsPath,
    Archive(archive::ArchiveError),
    Http(reqwest::Error),
    ApiCallFailure {
        status: StatusCode,
        body: String,
        api: ApiKind,
    },
    // TODO Change to a common ApiCallFailure with an enum to discriminate the api
    UploadRequestFailed {
        status: StatusCode,
        reason: String,
    },
    UploadFailed {
        status: StatusCode,
        reason: String,
    },
    MetadaDataUpdateFailed {
        status: StatusCode,
        reason: String,
    },
}

impl From<archive::ArchiveError> for Error {
    fn from(error: archive::ArchiveError) -> Self {
        Error::Archive(error)
    }
}

impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        Error::Http(error)
    }
}

#[derive(Debug)]
pub enum ApiKind {
    RenewToken,
    Register,
    ListDocuments,
}

pub struct DeviceId(String); // uuid
pub struct Token(String);

pub struct Client {
    http: reqwest::Client,
    device_token: Option<Token>,
    user_token: Option<Token>,
}

pub fn make_client() -> Result<Client, Error> {
    let http = reqwest::Client::new();

    let device_token = std::env::var("DEVICE_TOKEN").ok().map(Token);

    Ok(Client {
        http,
        device_token,
        user_token: None,
    })
}

fn validate_file_name_for_upload(file_name: &str) -> Result<(String, String), Error> {
    let path = Path::new(file_name);

    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .ok_or(Error::NoValidExtensionForUpload(None))?;

    // ext != epub && ext != pdf
    if !(ext == "epub" || ext == "pdf") {
        return Err(Error::NoValidExtensionForUpload(Some(ext.to_owned())));
    }

    if file_name.contains(std::path::MAIN_SEPARATOR) {
        return Err(Error::FileNameIsPath);
    }

    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or(Error::NoValidFileNameForUpload)?;

    Ok((name.to_owned(), ext.to_owned()))
}

impl Client {
    // TODO Extract as a function which take a Client instead ?
    /// Upload a pdf/epub document to the remarkable cloud.
    ///
    /// It is required to know the document id of the folder where the file
    /// will be uploaded under.
    pub async fn upload_epub(
        &self,
        content: &Vec<u8>,
        file_name: &str,
        folder: DocumentId,
    ) -> Result<(), Error> {
        // 1. Check the file name and extension is supported
        let (name, ext) = validate_file_name_for_upload(&file_name)?;

        let doc_id = DocumentId::new();

        // 2. Create the remarkable archive (file format at https://remarkablewiki.com/tech/filesystem#metadata_file_format)
        let archive = archive::make(&doc_id, &ext, content)?;

        let mut file = tokio::fs::File::create("/Users/francoismonniot/chapter.zip")
            .await
            .unwrap();
        file.write_all(&archive).await.unwrap();

        // 3. Send an upload request
        let uploads = self.upload_request(&doc_id, EntryType::Document).await?;
        let upload = &uploads[0]; // safe because we would error above if not one available, I think

        // 4. Send the archive to the url obtained in the previous step
        self.upload_archive(&upload.blob_url_put, archive).await?;

        // 5. Update the metadata to make the file visible
        self.update_metadata(doc_id, folder, name, EntryType::Document)
            .await?;

        Ok(())
    }

    pub async fn renew_token(&mut self) -> Result<(), Error> {
        debug!("Attempt to renew user token");
        let token = self.device_token.as_ref().ok_or(Error::NoTokenAvailable)?;

        let response = self
            .http
            .post("https://my.remarkable.com/token/json/2/user/new")
            .bearer_auth(&token.0)
            .header("content-length", 0) // rmcloud requires it and reqwest doesn't set it when value is 0
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;

        if status.is_success() {
            self.user_token.replace(Token(body));

            Ok(())
        } else {
            Err(Error::ApiCallFailure {
                status,
                body,
                api: ApiKind::RenewToken,
            })
        }
    }

    /// If no token has been found in the initial configuration,
    /// then the client will automatically try to create a new
    /// token by registering a new desktop app.
    ///
    /// As this require the user to give back a registration code,
    /// this method should not be used in an automated context.
    // TODO Manage errors correctly
    #[allow(unused)]
    pub async fn register(&mut self, code: &str) -> Result<(), Error> {
        debug!("Attempt to register a new device code");
        let did = Uuid::new_v4().to_string();

        let payload = json!({
            "code": code,
            "deviceDesc": "desktop-windows",
            "deviceID": did,
        });

        let response = self
            .http
            .post("https://my.remarkable.com/token/json/2/device/new")
            .json(&payload)
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;

        if status.is_success() {
            self.device_token.replace(Token(body));

            Ok(())
        } else {
            Err(Error::ApiCallFailure {
                status,
                body,
                api: ApiKind::Register,
            })
        }
    }

    pub async fn list_documents(&self) -> Result<Vec<Document>, Error> {
        debug!("Listing user documents");
        let token = self.user_token.as_ref().ok_or(Error::NoTokenAvailable)?;

        let response = self
            .http
            .get(DOCUMENT_LIST_URL)
            .bearer_auth(&token.0)
            .send()
            .await?;

        let status = response.status();

        if status.is_success() {
            Ok(response.json().await?)
        } else {
            let body = response.text().await?;

            Err(Error::ApiCallFailure {
                status,
                body,
                api: ApiKind::ListDocuments,
            })
        }
    }

    async fn upload_request(
        &self,
        doc_id: &DocumentId,
        entry_type: EntryType,
    ) -> Result<Vec<UploadRequestResponse>, Error> {
        debug!("Creating upload request for document {:?}", doc_id);

        let token = self.user_token.as_ref().ok_or(Error::NoTokenAvailable)?;
        let payload = json!([{
            "ID": doc_id.0,
            "Type": entry_type.as_str(),
            "Version": 1 // We only support new documents for now
        }]);

        let response = self
            .http
            .put(DOCUMENT_UPLOAD_URL)
            .header("User-Agent", "rmsync")
            .bearer_auth(&token.0)
            .json(&payload)
            .send()
            .await?;

        let status = response.status();

        if status.is_success() {
            let body = response.json().await?;
            debug!("upload_request:response.body={:#?}", body);

            Ok(body)
        } else {
            let reason = response.text().await?;

            Err(Error::UploadRequestFailed { status, reason })
        }
    }

    async fn upload_archive(&self, url: &str, archive: Vec<u8>) -> Result<(), Error> {
        debug!("Uploading archive to the reMarkable cloud");

        // No need for authentication here as its already part of the url
        let response = self
            .http
            .put(url)
            .header("User-Agent", "rmsync")
            .body(archive)
            .send()
            .await?;

        let status = response.status();
        let headers = response.headers().clone();
        let reason = response.text().await?;

        debug!("upload_archive:response.status={}", status);
        debug!("upload_archive:response.body='{}'", reason);
        debug!("upload_archive:response.headers='{:?}'", headers);

        if status.is_success() {
            Ok(())
        } else {
            Err(Error::UploadFailed { status, reason })
        }
    }

    async fn update_metadata(
        &self,
        doc_id: DocumentId,
        parent: DocumentId,
        name: String,
        entry_type: EntryType,
    ) -> Result<(), Error> {
        debug!("Creating metadata for document id {}", doc_id.0);

        let token = self.user_token.as_ref().ok_or(Error::NoTokenAvailable)?;
        let payload = json!([{
            "ID":             doc_id.0,
            "Parent":         parent.0,
            "VissibleName":   name,
            "Type":           entry_type.as_str(),
            "Version":        1,
            "ModifiedClient": Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true),
        }]);

        let response = self
            .http
            .put(DOCUMENT_UPDATE_URL)
            .header("User-Agent", "rmsync")
            .bearer_auth(&token.0)
            .json(&payload)
            .send()
            .await?;

        let status = response.status();
        let reason = response.text().await?;

        debug!("upload_request:response.status={:?}", status);
        debug!("upload_request:response.body={:#?}", reason);

        if status.is_success() {
            Ok(())
        } else {
            Err(Error::MetadaDataUpdateFailed { status, reason })
        }
    }
}

#[derive(Deserialize, Debug)]
pub struct DocumentId(String);

impl DocumentId {
    pub fn new() -> DocumentId {
        DocumentId(Uuid::new_v4().to_string())
    }

    pub fn known(s: &str) -> DocumentId {
        DocumentId(s.to_string())
    }

    /// This is a special case that should be only used in one instance:
    /// when creating a document at the root (so not in a folder), use this
    /// document id for the parent.
    pub fn empty() -> DocumentId {
        DocumentId("".to_owned())
    }
}

// TODO Remove the two pub modifier and use accessors instead
#[derive(Deserialize, Debug)]
pub struct Document {
    #[serde(rename = "ID")]
    pub id: DocumentId,
    #[serde(rename = "Version")]
    version: u16,
    #[serde(rename = "Message")]
    message: String,
    #[serde(rename = "Success")]
    success: bool,
    #[serde(rename = "BlobURLGet")]
    blob_url_get: String,
    #[serde(rename = "BlobURLGetExpires")]
    blob_url_get_expires: String,
    #[serde(rename = "ModifiedClient")]
    modified_client: String,
    #[serde(rename = "Type")]
    pub tpe: String,
    #[serde(rename = "VissibleName")]
    pub visible_name: String,
    #[serde(rename = "CurrentPage")]
    current_page: u16,
    #[serde(rename = "Bookmarked")]
    bookmarked: bool,
    #[serde(rename = "Parent")]
    parent: DocumentId,
}

enum EntryType {
    #[allow(unused)]
    Collection,
    Document,
}

impl EntryType {
    fn as_str(&self) -> &str {
        match self {
            EntryType::Collection => "CollectionType",
            EntryType::Document => "DocumentType",
        }
    }
}

#[derive(Deserialize, Debug)]
struct UploadRequestResponse {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "Version")]
    version: u32,
    #[serde(rename = "Message")]
    message: String,
    #[serde(rename = "Success")]
    success: bool,
    #[serde(rename = "BlobURLPut")]
    blob_url_put: String,
    #[serde(rename = "BlobURLPutExpires")]
    blob_url_put_expires: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_name_validation() {
        assert_eq!(
            validate_file_name_for_upload("file_name.epub").unwrap(),
            ("file_name".to_string(), "epub".to_string())
        );

        assert_eq!(
            validate_file_name_for_upload("my great book.pdf").unwrap(),
            ("my great book".to_string(), "pdf".to_string())
        );

        // invalid extension
        match validate_file_name_for_upload("file_name.gz") {
            Err(Error::NoValidExtensionForUpload(Some(ext))) => assert_eq!(ext, "gz".to_string()),
            res => panic!("unexpected result: {:?}", res),
        }

        // path instead of file
        match validate_file_name_for_upload("my/file_name.epub") {
            Err(Error::FileNameIsPath) => (),
            res => panic!("unexpected result: {:?}", res),
        }
    }
}
