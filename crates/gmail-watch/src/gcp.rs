use crate::tokens::UserToken;
use gcp_auth::Token;
use log::{debug, warn};
use mime::Mime;
use serde::Deserialize;
use serde_json::json;

// TODO It'd be nice to share the AuthenticationManager and reqwest::Client between connections
pub async fn make_client(
    project_id: String,
    client_id: String,
    client_secret: String,
) -> Result<GcpClient, Error> {
    let authentication_manager = gcp_auth::init().await?;
    let token = authentication_manager
        .get_token(&["https://www.googleapis.com/auth/datastore"])
        .await?;
    let http = reqwest::Client::new();

    Ok(GcpClient {
        project_id,
        client_id,
        client_secret,
        token,
        http,
    })
}

#[derive(Debug)]
pub enum Error {
    Json(serde_json::Error),
    Http(reqwest::Error),
    GcpAuth(gcp_auth::Error),
    Base64(base64::DecodeError),
    Utf8(std::string::FromUtf8Error),

    InvalidDatastoreContent(String),
    EmailWithoutFromField,
    MissingValidMultipartContentType,
    NoBodyToDecode, // when a gmail::Message.data field is absent and tried to decode it
}

// TODO See if it's still needed with thiserror or anyhow
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SuperError is here!")
    }
}

// TODO See if it's still needed with thiserror or anyhow
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Error::Json(error)
    }
}

impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        Error::Http(error)
    }
}

impl From<gcp_auth::Error> for Error {
    fn from(error: gcp_auth::Error) -> Self {
        Error::GcpAuth(error)
    }
}
impl From<base64::DecodeError> for Error {
    fn from(error: base64::DecodeError) -> Self {
        Error::Base64(error)
    }
}
impl From<std::string::FromUtf8Error> for Error {
    fn from(error: std::string::FromUtf8Error) -> Self {
        Error::Utf8(error)
    }
}

pub struct GcpClient {
    project_id: String,
    client_id: String,
    client_secret: String,
    token: Token,
    http: reqwest::Client,
}

// TODO Implement a better error system. I will need the response content in the logs
// when a request goes wrong. Otherwise I know myself and won't put the effort to
// investiguate further.
impl GcpClient {
    pub async fn cloud_datastore_user_by_email(
        &self,
        email: &str,
    ) -> Result<DatastoreLookup, Error> {
        let res = self
            .http
            .post(&format!(
                "https://datastore.googleapis.com/v1/projects/{}:lookup?alt=json",
                self.project_id
            ))
            .bearer_auth(self.token.as_str())
            .json(&json!({
              "keys": [
                {
                  "partitionId": {
                    "projectId": self.project_id
                  },
                  "path": [
                    {
                      "kind": "oauth2token",
                      "name": email
                    }
                  ]
                }
              ]
            }))
            .send()
            .await?;

        let result: datastore::LookupResult = res.json().await?;

        match result {
            datastore::LookupResult::Found { found } => match found.first() {
                None => Ok(DatastoreLookup::Missing),
                Some(result) => {
                    let token = result
                        .entity
                        .properties
                        .get("token")
                        .and_then(|v| v.string_value.as_ref());

                    match token {
                        None => Err(Error::InvalidDatastoreContent(
                            "Missing token in datastore entity".to_string(),
                        )),
                        Some(t) => Ok(DatastoreLookup::Found {
                            token: t.to_string(),
                        }),
                    }
                }
            },
            datastore::LookupResult::Missing { .. } => Ok(DatastoreLookup::Missing),
        }
    }

    // https://cloud.google.com/identity-platform/docs/use-rest-api#section-refresh-token
    pub async fn refresh_user_token(&self, token: &mut UserToken) -> Result<(), Error> {
        debug!("Refreshing user token");

        let mut form = std::collections::HashMap::new();
        form.insert("client_id", self.client_id.as_str());
        form.insert("client_secret", self.client_secret.as_str());
        form.insert("grant_type", "refresh_token");
        form.insert("refresh_token", token.refresh_token());

        let res = self
            .http
            .post("https://oauth2.googleapis.com/token")
            .form(&form)
            .send()
            .await?;

        let new_token: identity::RefreshTokenResponse = res.json().await?;

        token.set_access_token(new_token.access_token);

        Ok(())
    }

    // https://developers.google.com/gmail/api/reference/rest/v1/users.history/list
    pub async fn gmail_users_history_list(
        &self,
        token: &UserToken,
        history_id: HistoryId,
    ) -> Result<Vec<MessageId>, Error> {
        debug!("Fetching user history list");

        let res = self
            .http
            .get("https://gmail.googleapis.com/gmail/v1/users/me/history")
            .bearer_auth(&token.as_str())
            .query(&[("startHistoryId", history_id.0)])
            .send()
            .await?;

        let result: gmail::HistoryListResponse = res.json().await?;

        Ok(result
            .history
            .into_iter()
            .flat_map(|h| h.messages)
            .map(|h| h.id)
            .collect())
    }

    // https://developers.google.com/gmail/api/reference/rest/v1/users.messages/get
    pub async fn gmail_get_message(&self, message_id: &MessageId) -> Result<EmailMessage, Error> {
        debug!("Fetching message {}", message_id.0);

        let res = self
            .http
            .get(&format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}",
                id = message_id.0
            ))
            .bearer_auth(self.token.as_str())
            .send()
            .await?;

        let status = res.status();
        let body = res.text().await?;

        match serde_json::from_str::<gmail::Message>(&body) {
            Ok(result) => {
                let from_header = result
                    .payload
                    .headers
                    .iter()
                    .find(|h| h.name == "From")
                    .ok_or(Error::EmailWithoutFromField)?;
                let from = from_header.value.clone();

                // TODO Find the correct body part and extract the mail's body from it

                Ok(EmailMessage { from })
            }
            Err(error) => {
                warn!(
                    "Couldn't fetch GMail message. status={} Response body =\n{}",
                    status, body
                );
                Err(error.into())
            }
        }
    }

    pub async fn gmail_get_messages<I: Iterator<Item = MessageId>>(
        &self,
        token: &UserToken,
        message_ids: I,
    ) -> Result<Vec<EmailMessage>, Error> {
        debug!("Fetching messages in batch");
        use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_LENGTH, CONTENT_TYPE};

        let req = self
            .http
            .post("https://www.googleapis.com/batch/gmail/v1")
            .bearer_auth(token.as_str());

        let res = multipart::gmail_get_messages_batch(req, message_ids.map(|i| i.0.clone()))
            .send()
            .await?;

        let h = res
            .headers()
            .get(CONTENT_TYPE)
            .ok_or(Error::MissingValidMultipartContentType)?;

        println!("content-type: {:?}", h);
        let mime = h.to_str().unwrap().parse::<Mime>().unwrap(); // TODO Errors

        // Might need convertion, let's see what httparse returns us
        let boundary = mime.get_param("boundary").unwrap().to_string(); // TODO Errors

        // content-type: "multipart/mixed; boundary=batch_47XVjsIfPXGWk00LSpbABytFrk9NfNT3"

        let response_body = res.bytes().await?;

        let s = response_body.clone().to_vec();
        let s = String::from_utf8(s).unwrap();

        println!("batch.response.boundary = {}", boundary);
        //println!("batch.response.body = |{}|", s);

        let responses = multipart::read_multipart_response(&boundary, response_body);

        let i: Vec<Result<EmailMessage, Error>> = responses
            .iter()
            .map(|r| {
                let m = serde_json::from_slice(r.body())?;
                let m = EmailMessage::from(m)?;

                Ok(m)
            })
            .collect();

        let mut messages = Vec::new();
        for e in i {
            match e {
                Ok(e) => messages.push(e),
                Err(e) => warn!("Couldn't parse email: {:?}", e),
            }
        }

        Ok(messages)
    }
}

#[derive(Debug)]
pub enum DatastoreLookup {
    // For now we only return the token
    Found { token: String },
    Missing,
}

/// A simplified version of gmail's [Message](gmail::Message)
#[derive(Debug)]
pub struct EmailMessage {
    pub from: String,
    //body: String,
}

impl EmailMessage {
    fn from(message: gmail::Message) -> Result<EmailMessage, Error> {
        let from_header = message
            .payload
            .headers
            .iter()
            .find(|h| h.name == "From")
            .ok_or(Error::EmailWithoutFromField)?;
        let from = from_header.value.clone();

        // TODO Find the correct body part and extract the mail's body from it

        Ok(EmailMessage { from })
    }
}

/// data structure used on the wire by Cloud Datastore APIs
mod datastore {
    use serde::Deserialize;
    use std::collections::HashMap;

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(untagged)]
    pub(super) enum LookupResult {
        Found { found: Vec<EntityResult> },
        Missing { missing: Vec<EntityResult> },
    }

    #[derive(Debug, Deserialize, PartialEq)]
    pub(super) struct EntityResult {
        pub(super) entity: Entity,
        pub(super) version: String,
        pub(super) cursor: Option<String>,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    pub(super) struct Entity {
        pub(super) key: Key,
        #[serde(default)]
        pub(super) properties: HashMap<String, Value>,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct Key {
        pub(super) partition_id: PartitionId,
        pub(super) path: Vec<PathElement>,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct PartitionId {
        pub(super) namespace: Option<String>,
        pub(super) project_id: String,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    pub(super) struct PathElement {
        pub(super) kind: String,
        pub(super) name: String,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct Value {
        pub(super) string_value: Option<String>,
        pub(super) array_value: Option<ArrayValue>,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    pub(super) struct ArrayValue {
        pub(super) values: Vec<Value>,
    }
}

/// data structure used on the wire by GMail APIs
mod gmail {
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct HistoryListResponse {
        pub(super) history: Vec<History>,
        next_page_token: Option<String>,
        history_id: String,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct History {
        id: String,
        pub(super) messages: Vec<HistoryMessage>,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct HistoryMessage {
        pub(super) id: super::MessageId,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct Message {
        pub(super) payload: MessagePart,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    pub(super) struct MessagePart {
        pub(super) headers: Vec<Header>,
        pub(super) body: MessagePartBody,
        #[serde(default)]
        pub(super) parts: Vec<MessagePart>,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    pub(super) struct Header {
        pub(super) name: String,
        pub(super) value: String,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    pub(super) struct MessagePartBody {
        size: u32,
        data: Option<String>, // TODO This is apparently optional in some cases
    }

    impl MessagePartBody {
        pub(super) fn decoded_data(&self) -> Result<String, super::Error> {
            if self.size == 0 {
                Err(base64::DecodeError::InvalidLength)?;
            }
            let data = self.data.as_ref().ok_or(super::Error::NoBodyToDecode)?;
            let bytes = base64::decode(data)?;
            let s = String::from_utf8(bytes)?;

            Ok(s)
        }
    }
}

mod identity {
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    pub(super) struct RefreshTokenResponse {
        pub(super) access_token: String,
        pub(super) expires_in: u32,
        pub(super) token_type: String,
        pub(super) scope: String,
    }
}

/// A trim down implemenation of `multipart/` request and responses.
///
/// It handles enough to make batch request to GMail. It is a non-goal
/// to handle all the complexity of the multipart specification.
#[allow(unused)]
mod multipart {

    use bytes::{Buf,Bytes};
    use httparse::{parse_headers, EMPTY_HEADER};
    use log::{debug, error};
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_LENGTH, CONTENT_TYPE};
    use reqwest::{Client, RequestBuilder, Response};

    // Because implementing a generic http encoder is way too much rust (the one in hyper
    // is 100+ lines long and manage a lot of edge cases). Given that all I need is to include
    // the snippet
    // ```txt
    // --batch_foobarbaz
    // Content-Type: application/http
    // Content-ID: <item1:12930812@barnyard.example.com>
    //
    // GET /farm/v1/animals/pony
    //
    // ...
    // ```
    // I'm just going to do it manually. It would be helpful if in the future there would
    // be a `httpencode` crate, like there is a `httparse` one
    pub(super) fn gmail_get_messages_batch<I: Iterator<Item = String>>(
        builder: RequestBuilder,
        ids: I,
    ) -> RequestBuilder {
        const BOUNDARY: &str = "batch_foobarbaz";
        let mut body = Vec::new();

        for id in ids {
            let string = format!(
                "--{boundary}\n\
              Content-Type: application/http\n\
              Content-Id: {id}\n\
              \n\
              GET /gmail/v1/users/me/messages/{id}\n\
              \n",
                boundary = BOUNDARY,
                id = id,
            );
            body.extend(string.as_bytes());
        }

        body.extend(format!("--{}--", BOUNDARY).as_bytes());

        builder
            .header(CONTENT_TYPE, "multipart/mixed; boundary=batch_foobarbaz")
            .header(CONTENT_LENGTH, body.len())
            .body(body)
    }


    #[derive(Debug, thiserror::Error)]
    enum ReadMultipartError {
        #[error("Can't parse HTTP headers: {0}")]
        Httparse(#[from] httparse::Error),
        #[error("No status code was found in the response")]
        NoStatusCodeDefined,
    }

    /// Given a `multipart/mixed` response body, parse each part of the response
    /// and return each response as hyper's [`Response<Bytes>`](hyper::Response).
    pub(super) fn read_multipart_response(
        boundary: &str,
        body: Bytes,
    ) -> Vec<hyper::Response<Bytes>> {
        let reader = MultipartReader::new(body, boundary);
        let mut responses = Vec::new();

        // There should only be Content-Type and Content-Length as the part headers
        const HEADER_LEN: usize = 2;

        // TODO Use as an iterator + map instead
        for mut raw_response in reader {
            let r = parse_multipart_response(raw_response);

            responses.push(r);
        }

        responses
    }

    // TODO Error management
    
    /// Given a part of a `multipart/mixed` body, parse the part headers and its body
    /// if the `Content-Type` is `application/html`.
    fn parse_multipart_response(mut raw_response: Bytes) -> hyper::Response<Bytes> {
        // There should only be Content-Type and Content-Length as the part headers
        const HEADER_LEN: usize = 2;

        // We just want to find out the end of the part headers, so we read them using
        // httparse::parse_headers and discard the result (we know there are only two of them)
        let mut raw_headers = [EMPTY_HEADER; HEADER_LEN];
        match parse_headers(&raw_response, &mut raw_headers).unwrap() {
            httparse::Status::Partial => {
                panic!("Partial header on a finite body, something went wrong")
            }
            httparse::Status::Complete((end_headers, _)) => {
                raw_response.advance(end_headers);

                // TODO Fails if Content-Type isn't application/html

                let (builder, body_position) = parse_http_response(raw_response.clone()).unwrap();

                raw_response.advance(body_position);

                let b = builder.body(raw_response).unwrap();

                return b;
            }
        }
    }

    // TODO Error management
    /// Parse the status-line and headers of a HTTP response returning a hyper's builder
    /// populated with the parsed information as well as the position within the `Bytes`
    /// where the response body start.
    fn parse_http_response(raw_response: Bytes) -> Result<(hyper::http::response::Builder, usize), ReadMultipartError> {
        let r: &[u8] = &raw_response;
        let mut h = [httparse::EMPTY_HEADER; 10];
        let mut response = httparse::Response::new(&mut h);
        
        let body_position = match response.parse(r)? {
            httparse::Status::Partial => {
                error!("Partial header on a finite body, something went wrong");
                0
            }
            httparse::Status::Complete(p) => p,
        };

        let mut builder = hyper::Response::builder().status(response.code.ok_or(ReadMultipartError::NoStatusCodeDefined)?);

        for h in response.headers {
            builder = builder.header(h.name, h.value);
        }

        Ok((builder, body_position))
    }

    // TODO Rename functions and test names. They don't make a lot of sense right now.

    struct MultipartReader {
        bytes: Bytes,
        position: usize,
        boundary: Bytes,
        boundary_end: Bytes,
    }

    const BOUNDARY_SEP: &[u8] = "--".as_bytes();
    const NEWLINE: &[u8] = "\r\n".as_bytes(); // HTTP use CRLF as newline character

    impl MultipartReader {
        fn new(bytes: Bytes, boundary: &str) -> MultipartReader {
            let boundary_bytes = boundary.as_bytes();
            let start = [NEWLINE, BOUNDARY_SEP, boundary_bytes, NEWLINE].concat();
            let boundary = Bytes::copy_from_slice(&start);
            let end = [NEWLINE, BOUNDARY_SEP, boundary_bytes, BOUNDARY_SEP, NEWLINE].concat();
            let boundary_end = Bytes::copy_from_slice(&end);

            MultipartReader {
                bytes,
                position: 0,
                boundary,
                boundary_end,
            }
        }

        // The whole algorimth assume we have the data in memory and try to not copy said data.
        // We might want to use a stream of data instead, let's see how it goes.
        fn next_part(&mut self) -> Option<Bytes> {
            // This clone will only clone the pointer to the underlying memory, not the data itself
            let mut bytes = self.bytes.clone();

            // Let's resume where we left off on the last iteration
            bytes.advance(self.position);

            debug!(
                "next: self.position: {}; remaining: {}",
                self.position,
                bytes.remaining()
            );

            // Nothing remains in the buffer so we are done looking into the multipart content
            if !bytes.has_remaining() {
                return None;
            }

            // We have to keep track of the position within the Buf ourselves
            let mut position = self.position;
            let mut boundary_found = false;
            let mut boundary_offset = 0;

            loop {
                // We are pointing at the start of a boundary
                if bytes.remaining() >= self.boundary.len()
                    && &bytes[0..self.boundary.len()] == self.boundary
                {
                    boundary_found = true;
                    boundary_offset = self.boundary.len();
                    break;
                }

                // We are at the end of the body, looking for the ending boundary
                if bytes.remaining() == self.boundary_end.len()
                    && &bytes[0..self.boundary_end.len()] == self.boundary_end
                {
                    boundary_found = true;
                    boundary_offset = self.boundary_end.len();
                    break;
                }

                // stop gap if no boundary have been found and the body is all read
                if !bytes.has_remaining() {
                    break;
                }

                bytes.advance(1);
                position += 1;
            }

            debug!(
                "after loop: position: {}; boundary_found: {}; boundary_offset: {}",
                position, boundary_found, boundary_offset
            );

            if boundary_found {
                if position == 0 {
                    // special case, on the very first loop the boundary is found first so we have no
                    // part to return yet. So we advance the buffer by the boundary and do another run.
                    // This is to avoid returning an empty Bytes on the first call.
                    self.position += boundary_offset;
                    return self.next();
                }

                let slice = self.bytes.slice(self.position..position);
                self.position = position + boundary_offset;

                Some(slice)
            } else {
                None
            }
        }
    }

    impl Iterator for MultipartReader {
        type Item = Bytes;

        fn next(&mut self) -> Option<Self::Item> {
            self.next_part()
        }
    }


    #[cfg(test)]
    #[test]
    fn test_parse_http_response_parts() {
        let not_modified = "HTTP/1.1 304 Not Modified\n\
              ETag: \"etag/animals\"\n\
              \n\
              ";
        let raw_response = Bytes::from_static(not_modified.as_bytes());

        let (builder, remaining) = parse_http_response(raw_response.clone()).expect("can parse");
        let response = builder.body(()).unwrap();

        let ok = hyper::StatusCode::from_u16(200).unwrap();
        let etag_value = hyper::header::HeaderValue::from_static("\"etag/animals\"");

        assert_eq!(remaining, raw_response.len());
        assert_eq!(response.status(), hyper::StatusCode::from_u16(304).unwrap());
        assert_eq!(response.headers().get("ETag"), Some(&etag_value));
    }

    #[cfg(test)]
    #[test]
    fn test_read_http_response() {
        let headers = "Content-Type: application/http\n\
            Content-ID: response-{id}\n\
            \n\
            HTTP/1.1 200 OK\n\
            Content-Type: application/json\n\
            Content-Length: 156\n\
            \n\
              ";
        let body = "{\n\
            \"kind\": \"farm#animal\",\n\
            \"etag\": \"etag/pony\",\n\
            \"selfLink\": \"/farm/v1/animals/pony\",\n\
            \"animalName\": \"pony\",\n\
            \"animalAge\": 34,\n\
            \"peltColor\": \"white\"\n\
          }\n\
          ";
        let raw_body = Bytes::from(body.as_bytes());
        let raw_response = [headers, body].concat();
        let raw_response = Bytes::copy_from_slice(raw_response.as_bytes());

        let response = parse_multipart_response(raw_response);

        let ok = hyper::StatusCode::from_u16(200).unwrap();
        let ct_value = hyper::header::HeaderValue::from_static("application/json");
        let cl_value = hyper::header::HeaderValue::from_static("156");

        assert_eq!(response.status(), hyper::StatusCode::from_u16(200).unwrap());
        assert_eq!(response.headers().get("Content-Type"), Some(&ct_value));
        assert_eq!(response.headers().get("Content-Length"), Some(&cl_value));

        assert_eq!(response.body(), &raw_body);
    }

    #[cfg(test)]
    #[test]
    fn test_read_parts() {
        let boundary = "boundary";
        let raw =  "\r\n--boundary\r\n\
                Content-ID: response-{id1}\n\
                \n\
                HTTP/1.1 200 OK\n\
                \n\
                \r\n--boundary\r\n\
                Content-ID: response-{id22}\n\
                \n\
                HTTP/1.1 200 OK\n\
                \n\
                \r\n--boundary\r\n\
                Content-ID: response-{id333}\n\
                \n\
                HTTP/1.1 200 OK\n\
                \n\
                \r\n--boundary--\r\n\
              ";

        let bytes = Bytes::from(raw.as_bytes());
        let mut reader = MultipartReader::new(bytes, boundary).map(|b| b.len());

        assert_eq!(reader.next(), Some(45));
        assert_eq!(reader.next(), Some(46));
        assert_eq!(reader.next(), Some(47));
        assert_eq!(reader.next(), None);
        assert_eq!(reader.next(), None);
    }
}

#[derive(Debug, Deserialize)]
pub struct HistoryId(pub String);

#[derive(Debug, Deserialize, PartialEq)]
pub struct MessageId(String);

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn asset(p: &str) -> String {
        let mut d = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        d.push("assets");
        d.push(p);

        std::fs::read_to_string(d).unwrap()
    }

    fn entity_result(properties: HashMap<String, datastore::Value>) -> datastore::EntityResult {
        let entity = datastore::Entity {
            key: datastore::Key {
                partition_id: datastore::PartitionId {
                    namespace: None,
                    project_id: "pid".to_string(),
                },
                path: vec![datastore::PathElement {
                    kind: "oauth2token".to_string(),
                    name: "my@gmail.com".to_string(),
                }],
            },
            properties,
        };

        datastore::EntityResult {
            entity,
            version: "2907302240639813".to_string(),
            cursor: None,
        }
    }

    #[test]
    fn deserialize_datastore_found() {
        let body = asset("test_datastore_found_response.json");

        let actual: datastore::LookupResult = serde_json::from_str(&body).unwrap();

        let mut properties = HashMap::new();

        properties.insert(
            "token".to_string(),
            datastore::Value {
                string_value: Some("LvrNooPprYvSiVwyN3VRIARnc05Pte/dtENtlLpWPZ7cC0O".to_string()),
                array_value: None,
            },
        );

        properties.insert(
            "scopes".to_string(),
            datastore::Value {
                string_value: None,
                array_value: Some(datastore::ArrayValue {
                    values: vec![
                        datastore::Value {
                            string_value: Some("email".to_string()),
                            array_value: None,
                        },
                        datastore::Value {
                            string_value: Some("profile".to_string()),
                            array_value: None,
                        },
                    ],
                }),
            },
        );

        let expected = datastore::LookupResult::Found {
            found: vec![entity_result(properties)],
        };

        assert_eq!(actual, expected);
    }

    #[test]
    fn deserialize_datastore_missing() {
        let body = asset("test_datastore_missing_response.json");

        let actual: datastore::LookupResult = serde_json::from_str(&body).unwrap();

        let expected = datastore::LookupResult::Missing {
            missing: vec![entity_result(HashMap::new())],
        };

        assert_eq!(actual, expected);
    }
}
