use bytes::buf::BufExt as _;
use hyper::client::HttpConnector;
use hyper::{Body, Request, StatusCode};
use hyper_tls::HttpsConnector;
use log::debug;
use serde::Deserialize;
use serde_json::json;
use std::path::Path;
use uuid::Uuid;
use chrono::Utc;
use tokio::prelude::*;

mod archive;

const DOCUMENT_LIST_URL: &str = "https://document-storage-production-dot-remarkable-production.appspot.com/document-storage/json/2/docs";
const DOCUMENT_UPDATE_URL: &str = "https://document-storage-production-dot-remarkable-production.appspot.com/document-storage/json/2/upload/update-status";
const DOCUMENT_UPLOAD_URL: &str = "https://document-storage-production-dot-remarkable-production.appspot.com/document-storage/json/2/upload/request";
//const DOCUMENT_DELETE_URL: &str = "https://document-storage-production-dot-remarkable-production.appspot.com/document-storage/json/2/delete";

#[derive(Debug)]
pub enum Error {
    NoTokenAvailable,
    NoValidExtensionForUpload(Option<String>),
    NoValidFileNameForUpload,
    FileNameIsPath,
    Archive(archive::ArchiveError),
    // TODO Change to a common ApiCallFailure with an enum to discriminate the api
    UploadRequestFailed { status: StatusCode, reason: String },
    UploadFailed { status: StatusCode, reason: String },
    MetadaDataUpdateFailed { status: StatusCode, reason: String },
}

impl From<archive::ArchiveError> for Error {
    fn from(error: archive::ArchiveError) -> Self {
        Error::Archive(error)
    }
}

pub struct DeviceId(String); // uuid
pub struct Token(String);

pub struct Client {
    http: hyper::Client<HttpsConnector<HttpConnector>, Body>,
    device_token: Option<Token>,
    user_token: Option<Token>,
}

pub fn make_client() -> Result<Client, Error> {
    let https = hyper_tls::HttpsConnector::new();
    let http = hyper::Client::builder().build::<_, hyper::Body>(https);

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

        let mut file = tokio::fs::File::create("/Users/francoismonniot/chapter.zip").await.unwrap();
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

        let req = Request::builder()
            .method("POST")
            .uri("https://my.remarkable.com/token/json/2/user/new")
            .header("Authorization", format!("Bearer {}", token.0))
            .header("Content-Length", 0)
            .body(Body::empty())
            .expect("request builder");

        let mut response = self.http.request(req).await.unwrap();

        // TODO Check status

        let bytes = hyper::body::to_bytes(response.body_mut()).await.unwrap();
        let token = String::from_utf8(bytes.to_vec()).expect("response was not valid utf-8");

        self.user_token.replace(Token(token));

        Ok(())
    }

    /// If no token has been found in the initial configuration,
    /// then the client will automatically try to create a new
    /// token by registering a new desktop app.
    ///
    /// As this require the user to give back a registration code,
    /// this method should not be used in an automated context.
    // TODO Manage errors correctly
    // TODO Generate UUID instead of using a fixed (used) one
    #[allow(unused)]
    pub async fn register(&mut self, code: &str) -> Result<(), Error> {
        debug!("Attempt to register a new device code");
        let did = "701c3752-1025-4770-af43-5ddcfa4dabb2";

        let payload = json!({
            "code": code,
            "deviceDesc": "desktop-windows",
            "deviceID": did,
        });
        let payload = serde_json::to_string(&payload).unwrap();

        let req = Request::builder()
            .method("POST")
            .uri("https://my.remarkable.com/token/json/2/device/new")
            .body(Body::from(payload))
            .expect("request builder");

        let mut response = self.http.request(req).await.unwrap();

        let bytes = hyper::body::to_bytes(response.body_mut()).await.unwrap();
        let token = String::from_utf8(bytes.to_vec()).expect("response was not valid utf-8");

        self.device_token.replace(Token(token));

        Ok(())
    }

    pub async fn list_documents(&self) -> Result<Vec<Document>, Error> {
        debug!("Listing user documents");
        let token = self.user_token.as_ref().ok_or(Error::NoTokenAvailable)?;

        let req = Request::builder()
            .method("GET")
            .uri(DOCUMENT_LIST_URL)
            .header("Authorization", format!("Bearer {}", token.0))
            .body(Body::empty())
            .expect("request builder");

        let response = self.http.request(req).await.unwrap();

        let body = hyper::body::aggregate(response).await.unwrap();
        let documents: Vec<Document> = serde_json::from_reader(body.reader()).unwrap();

        Ok(documents)
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
        let payload = serde_json::to_string(&payload).unwrap();

        let req = Request::builder()
            .method("PUT")
            .uri(DOCUMENT_UPLOAD_URL)
            .header("Authorization", format!("Bearer {}", token.0))
            .body(Body::from(payload))
            .expect("request builder");

        let response = self.http.request(req).await.unwrap();
        let status = response.status();

        debug!("upload_request:response.status={}", status);

        if status.is_success() {
            let body = hyper::body::aggregate(response).await.unwrap();
            let response = serde_json::from_reader(body.reader()).unwrap();

            debug!("upload_request:response.body={:#?}", response);

            Ok(response)
        } else {
            let bytes = hyper::body::to_bytes(response.into_body()).await.unwrap();
            let reason = String::from_utf8(bytes.to_vec()).expect("response was not valid utf-8");

            Err(Error::UploadRequestFailed { status, reason })
        }
    }

    async fn upload_archive(&self, url: &str, archive: Vec<u8>) -> Result<(), Error> {
        debug!("Uploading archive to the reMarkable cloud");
        let token = self.user_token.as_ref().ok_or(Error::NoTokenAvailable)?;

        let req = Request::builder()
            .method("PUT")
            .uri(url)
            .header("Authorization", format!("Bearer {}", token.0))
            .header("User-Agent", "rmsync")
            .header("Accept-Encoding", "gzip")
            .header("Content-Length", archive.len())
            .body(Body::from(archive))
            .expect("request builder");

        let response = self.http.request(req).await.unwrap();
        let status = response.status();

        debug!("upload_archive:response.status={}", status);

        if status.is_success() {
            let headers = response.headers();
            debug!("upload_archive:response.headers='{:?}'", headers);

            let bytes = hyper::body::to_bytes(response.into_body()).await.unwrap();
            let content = String::from_utf8(bytes.to_vec()).expect("response was not valid utf-8");
            debug!("upload_archive:response.body='{}'", content);

            Ok(())
        } else {
            let bytes = hyper::body::to_bytes(response.into_body()).await.unwrap();
            let reason = String::from_utf8(bytes.to_vec()).expect("response was not valid utf-8");

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
        let payload = json!({
            "ID":             doc_id.0,
            "Parent":         parent.0,
            "VissibleName":   name,
            "Type":           entry_type.as_str(),
            "Version":        1,
            "ModifiedClient": Utc::now().to_rfc3339(),
        });
        let payload = serde_json::to_string(&payload).unwrap();
        debug!("update_metadata:request.body={}", payload);

        let req = Request::builder()
            .method("PUT")
            .uri(DOCUMENT_UPDATE_URL)
            .header("Authorization", format!("Bearer {}", token.0))
            .body(Body::from(payload))
            .expect("request builder");

        let response = self.http.request(req).await.unwrap();
        let status = response.status();

        debug!("update_metadata:response.status={}", status);

        if status.is_success() {
            let bytes = hyper::body::to_bytes(response.into_body()).await.unwrap();
            let content = String::from_utf8(bytes.to_vec()).expect("response was not valid utf-8");

            debug!("update_metadata:response.body='{}'", content);
            Ok(())
        } else {
            let bytes = hyper::body::to_bytes(response.into_body()).await.unwrap();
            let reason = String::from_utf8(bytes.to_vec()).expect("response was not valid utf-8");

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
