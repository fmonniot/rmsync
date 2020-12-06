use bytes::buf::BufExt as _;
use hyper::client::HttpConnector;
use hyper::{Body, Request};
use hyper_tls::HttpsConnector;
use log::debug;
use serde::Deserialize;
use serde_json::json;

const DOCUMENT_LIST_URL: &str = "https://document-storage-production-dot-remarkable-production.appspot.com/document-storage/json/2/docs";
const DOCUMENT_UPDATE_URL: &str = "https://document-storage-production-dot-remarkable-production.appspot.com/document-storage/json/2/upload/update-status";
const DOCUMENT_UPLOAD_URL: &str = "https://document-storage-production-dot-remarkable-production.appspot.com/document-storage/json/2/upload/request";
const DOCUMENT_DELETE_URL: &str = "https://document-storage-production-dot-remarkable-production.appspot.com/document-storage/json/2/delete";

#[derive(Debug)]
pub enum Error {
    NoTokenAvailable,
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

impl Client {
    pub async fn upload(&self) -> Result<(), Error> {
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

        /* With serde
        let body = hyper::body::aggregate(response).await.unwrap();
        let users = serde_json::from_reader(body.reader()).unwrap();
        */

        self.device_token.replace(Token(token));

        Ok(())
    }

    pub async fn list_documents(&self) -> Result<Vec<Document>, Error> {
        debug!("Attempt to list user's documents");
        let token = self.user_token.as_ref().ok_or(Error::NoTokenAvailable)?;

        debug!("user_token={}", token.0);

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
}

#[derive(Deserialize, Debug)]
pub struct DocumentId(String);

#[derive(Deserialize, Debug)]
pub struct Document {
    #[serde(rename = "ID")]
    id: DocumentId,
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
    tpe: String,
    #[serde(rename = "VissibleName")]
    visible_name: String,
    #[serde(rename = "CurrentPage")]
    current_page: u16,
    #[serde(rename = "Bookmarked")]
    bookmarked: bool,
    #[serde(rename = "Parent")]
    parent: DocumentId,
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
