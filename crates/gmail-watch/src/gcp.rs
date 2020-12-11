use crate::tokens::UserToken;
use gcp_auth::Token;
use log::{debug, warn};
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
    DecodeMultipart(multipart::ReadMultipartError),

    InvalidDatastoreContent(String),
    EmailWithoutFromField,
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
impl From<multipart::ReadMultipartError> for Error {
    fn from(error: multipart::ReadMultipartError) -> Self {
        Error::DecodeMultipart(error)
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

        let json = json!({"keys": [datastore::key(email, &self.project_id)]});

        debug!("datastore.request.body: {:?}", serde_json::to_string(&json));

        let res = self
            .http
            .post(&format!(
                "https://datastore.googleapis.com/v1/projects/{}:lookup?alt=json",
                self.project_id
            ))
            .bearer_auth(self.token.as_str())
            .json(&json)
            .send()
            .await?;

        debug!("datastore.response.status: {}", res.status());

        let result: datastore::LookupResult = res.json().await?;

        match result {
            datastore::LookupResult::Found { found } => match found.first() {
                None => Ok(DatastoreLookup::Missing),
                Some(result) => {

                    Ok(DatastoreLookup::Found(DatastoreUser::from_entity(&result.entity)?))
                }
            },
            datastore::LookupResult::Missing { .. } => Ok(DatastoreLookup::Missing),
        }
    }

    pub async fn cloud_datastore_update_history_id(
        &self,
        email: &str,
        user: &DatastoreUser
    ) -> Result<(), Error> {
        let response = self
            .http
            .post(&format!(
                "https://datastore.googleapis.com/v1/projects/{projectId}:beginTransaction",
                projectId = self.project_id
            ))
            .bearer_auth(self.token.as_str())
            .json(&json!({
                "transactionOptions": {
                    "readWrite": {

                    }
                }
            }))
            .send()
            .await?;

        let status = response.status();
        let body: datastore::BeginTransactionResponse = response.json().await?;

        debug!("beginTransaction: status:{}, body:{:?}", status, body);

        let key = datastore::key(email, &self.project_id);
        let entity = user.as_entity(key);

        let req = json!({
            "transaction": body.transaction,
            "mutations": [ { "update": entity } ]
        });

        let response = self
            .http
            .post(&format!(
                "https://datastore.googleapis.com/v1/projects/{projectId}:commit",
                projectId = self.project_id
            ))
            .bearer_auth(self.token.as_str())
            .json(&req)
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;

        debug!("commit: status:{}, body:|{:?}|", status, body);

        Ok(())
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
    // https://developers.google.com/gmail/api/guides/batch
    pub async fn gmail_get_messages<I: Iterator<Item = MessageId>>(
        &self,
        token: &UserToken,
        message_ids: I,
    ) -> Result<Vec<EmailMessage>, Error> {
        debug!("Fetching messages in batch");

        let request = self
            .http
            .post("https://www.googleapis.com/batch/gmail/v1")
            .bearer_auth(token.as_str());

        let response =
            multipart::gmail_get_messages_batch(request, message_ids.map(|i| i.0.clone()))
                .send()
                .await?;

        let responses: Vec<Result<EmailMessage, Error>> = multipart::read_response(response)
            .await?
            .into_iter()
            .map(|r| {
                let r = r?;
                let m = serde_json::from_slice(r.body())?;
                let e = EmailMessage::from(m)?;

                Ok(e)
            })
            .collect();

        let mut messages = Vec::new();
        for r in responses {
            match r {
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
    Found(DatastoreUser),
    Missing,
}

/// A representation of a user in Cloud Datastore
#[derive(Debug)]
pub struct DatastoreUser {
    pub token: String,
    scopes: Vec<String>,
    last_known_history_id: Option<u32>,
}

impl DatastoreUser {
    fn from_entity(entity: &datastore::Entity) -> Result<DatastoreUser, Error> {
        let token = entity
            .properties
            .get("token")
            .and_then(|v| v.string_value.as_ref())
            .cloned()
            .ok_or(Error::InvalidDatastoreContent(
                "Missing token in datastore entity".to_string(),
            ))?;

        let scopes: Vec<String> = entity.properties.get("scopes")
            .and_then(|v| v.array_value.as_ref())
            .map(|a| a.values.iter().flat_map(|v| v.string_value.as_ref()).cloned().collect())
            .ok_or(Error::InvalidDatastoreContent(
                "Missing scopes in datastore entity".to_string(),
            ))?;

        let last_known_history_id = entity
            .properties
            .get("history_id")
            .and_then(|v| v.as_u32());

        Ok(DatastoreUser {
            token,
            scopes,
            last_known_history_id,
        })
    }

    fn as_entity(&self, key: datastore::Key) -> datastore::Entity {
        let mut properties = std::collections::HashMap::new();
        
        properties.insert("token".to_string(), datastore::Value::new_string(&self.token));
        properties.insert("scopes".to_string(), datastore::Value::new_array(self.scopes.iter().map(|s| datastore::Value::new_string(s))));
        if let Some(id) = self.last_known_history_id {
            properties.insert("history_id".to_string(), datastore::Value::new_integer(id));
        }
        
        datastore::Entity {
            key, properties
        }
    }

    pub fn new_history(&mut self, history_id: &HistoryId) {
        self.last_known_history_id.replace(history_id.0);
    }

}

/// A simplified version of gmail's [Message](gmail::Message)
#[derive(Debug)]
pub struct EmailMessage {
    pub from: String,
    pub body: Option<String>,
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

        // This assume the message isn't multipart (as ff.net aren't)
        let body = message.payload.body.decoded_data().ok();

        Ok(EmailMessage { from, body })
    }
}

/// data structure used on the wire by Cloud Datastore APIs
mod datastore {
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;

    /// An helper to make the key we use in this project
    // TODO Return a Key instead ?
    pub(super) fn key(email: &str, project_id: &str) -> Key {
        Key {
            partition_id: PartitionId {
                namespace: None,
                project_id: project_id.to_string()
            },
            path: vec![
                PathElement {
                    kind: "oauth2token".to_string(),
                    name: email.to_string()
                }
            ]
        }
    }

    // Lookup API

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

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    pub(super) struct Entity {
        pub(super) key: Key,
        #[serde(default)]
        pub(super) properties: HashMap<String, Value>,
    }

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct Key {
        pub(super) partition_id: PartitionId,
        pub(super) path: Vec<PathElement>,
    }

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct PartitionId {
        #[serde(skip_serializing_if = "Option::is_none")]
        pub(super) namespace: Option<String>,
        pub(super) project_id: String,
    }

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    pub(super) struct PathElement {
        pub(super) kind: String,
        pub(super) name: String,
    }


    // TODO value field should actually be an enum and flatten it with serde
    // because we can only have one of those fields at the same time
    #[derive(Debug, Deserialize, PartialEq, Default, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct Value {
        // metadata
        #[serde(skip_deserializing)]
        //#[serde(skip_serializing_if = "ser_exclude_index")]
        exclude_from_indexes: bool,
        // values
        pub(super) string_value: Option<String>,
        pub(super) integer_value: Option<String>,
        pub(super) array_value: Option<ArrayValue>,
    }

    impl Value {
        pub(super) fn new_integer(v: u32) -> Value {
            Value {
                exclude_from_indexes: true,
                integer_value: Some(v.to_string()),
                ..Default::default()
            }
        }

        pub(super) fn new_string(v: &str) -> Value {
            Value {
                exclude_from_indexes: true,
                string_value: Some(v.to_string()),
                ..Default::default()
            }
        }

        pub(super) fn new_array<I: Iterator<Item= Value>>(i: I) -> Value {
            Value {
                exclude_from_indexes: true,
                array_value: Some(ArrayValue {
                    values: i.collect()
                }),
                ..Default::default()
            }
        }

        pub(super) fn as_u32(&self) -> Option<u32> {
            self.integer_value.as_ref().and_then(|s| s.parse::<u32>().ok())
        }

    }

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    pub(super) struct ArrayValue {
        pub(super) values: Vec<Value>,
    }

    // Beging Transaction

    #[derive(Debug, Deserialize, PartialEq)]
    pub(super) struct BeginTransactionResponse {
        pub(super) transaction: String,
    }
}

/// data structure used on the wire by GMail APIs
mod gmail {
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct HistoryListResponse {
        #[serde(default)]
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

    use bytes::{Buf, Bytes};
    use httparse::{parse_headers, EMPTY_HEADER};
    use log::{debug, error};
    use mime::Mime;
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
    pub enum ReadMultipartError {
        #[error("Can't parse HTTP headers: {0}")]
        Httparse(#[from] httparse::Error),

        #[error("HTTP malformed error: {0}")]
        Http(#[from] hyper::http::Error),

        #[error("Can't parse the MIME type: {0}")]
        Mime(#[from] mime::FromStrError),

        #[error("Can't read the header value as a string: {0}")]
        ReadHeaderValue(#[from] hyper::http::header::ToStrError),

        #[error("Can't read the HTTP response: {0}")]
        Reqwest(#[from] reqwest::Error),

        #[error("No status code was found in the response")]
        NoStatusCodeDefined,

        #[error("Partial header on a finite body, something went wrong (in {0})")]
        PartialHeaders(&'static str),

        #[error("The received response doesn't have a Content-Type header")]
        MissingContentType,

        #[error("The received response doesn't have a boundary delimiter")]
        NoBoundary,
    }

    type ReadResult<T> = Result<T, ReadMultipartError>;

    /// Given a response, and only if it has a `multipart/mixed` content type
    /// with a boundary set, parse the response body and return the individual
    /// responses (as hyper [`Response`](hyper::Response) not reqwest [`Response`](reqwest::Response)).
    pub(super) async fn read_response(
        response: reqwest::Response,
    ) -> ReadResult<Vec<ReadResult<hyper::Response<Bytes>>>> {
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .ok_or(ReadMultipartError::MissingContentType)?
            .to_str()?
            .parse::<Mime>()?;

        // Extract the boundary separator from the Content-Type
        let boundary = content_type
            .get_param("boundary")
            .ok_or(ReadMultipartError::NoBoundary)?
            .to_string();

        let response_body = response.bytes().await?;

        debug!("multipart.response.boundary = {}", boundary);

        Ok(read_response_body(&boundary, response_body))
    }

    /// Given a `multipart/mixed` response body, parse each part of the response
    /// and return each response as hyper's [`Response<Bytes>`](hyper::Response).
    pub(super) fn read_response_body(
        boundary: &str,
        body: Bytes,
    ) -> Vec<ReadResult<hyper::Response<Bytes>>> {
        MultipartReader::new(body, boundary)
            .map(parse_multipart_response)
            .collect()
    }

    /// Given a part of a `multipart/mixed` body, parse the part headers and its body
    /// if the `Content-Type` is `application/html`.
    fn parse_multipart_response(mut raw_response: Bytes) -> ReadResult<hyper::Response<Bytes>> {
        // There should only be Content-Type and Content-Length as the part headers
        const HEADER_LEN: usize = 2;

        // We just want to find out the end of the part headers, so we read them using
        // httparse::parse_headers and discard the result (we know there are only two of them)
        let mut raw_headers = [EMPTY_HEADER; HEADER_LEN];
        match parse_headers(&raw_response, &mut raw_headers)? {
            httparse::Status::Partial => {
                return Err(ReadMultipartError::PartialHeaders("part headers"));
            }
            httparse::Status::Complete((end_headers, _)) => {
                raw_response.advance(end_headers);

                // TODO Fails if Content-Type isn't application/html

                let (builder, body_position) = parse_http_response(raw_response.clone())?;

                raw_response.advance(body_position);

                let b = builder.body(raw_response)?;

                return Ok(b);
            }
        }
    }

    /// Parse the status-line and headers of a HTTP response returning a hyper's builder
    /// populated with the parsed information as well as the position within the `Bytes`
    /// where the response body start.
    fn parse_http_response(
        raw_response: Bytes,
    ) -> ReadResult<(hyper::http::response::Builder, usize)> {
        let r: &[u8] = &raw_response;
        let mut h = [httparse::EMPTY_HEADER; 10];
        let mut response = httparse::Response::new(&mut h);

        let body_position = match response.parse(r)? {
            httparse::Status::Partial => {
                return Err(ReadMultipartError::PartialHeaders("inner response"))
            }
            httparse::Status::Complete(p) => p,
        };

        let status = response
            .code
            .ok_or(ReadMultipartError::NoStatusCodeDefined)?;
        let mut builder = hyper::Response::builder().status(status);

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

        let response = parse_multipart_response(raw_response).expect("can parse response");

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
        let raw = "\r\n--boundary\r\n\
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
pub struct HistoryId(pub u32);

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

    use serde::{Serialize, Deserialize};

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Outer {
        #[serde(flatten)]
        value: Inner,
    }

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Props {
        properties: std::collections::HashMap<String, Outer>
    }

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    #[serde(untagged)]
    enum Inner {
        #[serde(rename="string_value")]
        #[serde(rename_all = "camelCase")]
        String {
            #[serde(skip_serializing_if = "Option::is_none")]
            exclude_from_indexes: Option<bool>,
            string_value: String,
        },
        #[serde(rename ="integer_value")]
        #[serde(rename_all = "camelCase")]
        Integer {
            #[serde(skip_serializing_if = "Option::is_none")]
            exclude_from_indexes: Option<bool>,
            integer_value: String,
        },
        #[serde(rename ="array_value")]
        #[serde(rename_all = "camelCase")]
        Array {
            array_value: InArray,
        }
    }

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct InArray {
        values: Vec<Outer>
    }

    #[test]
    fn test_flatten() {
        let out_str = Outer {
            value: Inner::String {
                exclude_from_indexes: Some(true),
                string_value: "LvrNooPprYvSiVwyN3VRIARnc05Pte/dtENtlLpWPZ7cC0O".to_string()
            }
        };
        let out_arr = Outer {
            value: Inner::Array {
                array_value: InArray {
                    values: vec![
                        Outer {
                            value: Inner::String {
                                exclude_from_indexes: None,
                                string_value: "email".to_string()
                            }
                        },
                        Outer {
                            value: Inner::String {
                                exclude_from_indexes: None,
                                string_value: "profile".to_string()
                            }
                        }
                    ]
                }
            }
        };

        let mut map = HashMap::new();
        map.insert("token".to_string(), out_str);
        map.insert("scopes".to_string(), out_arr);
        let props = Props {
            properties: map
        };
        println!("{}", serde_json::to_string(&props).unwrap());


        let raw = r#"{
            "properties": {
                "token": {
                    "stringValue": "LvrNooPprYvSiVwyN3VRIARnc05Pte/dtENtlLpWPZ7cC0O",
                    "excludeFromIndexes": true
                },
                "scopes": {
                    "arrayValue": {
                        "values": [
                            { "stringValue": "email" },
                            { "stringValue": "profile" }
                        ]
                    }
                }
            }
        }"#;

        println!("result: {:?}", serde_json::from_str::<Props>(raw));

    }

    #[test]
    fn deserialize_datastore_found() {
        let body = asset("test_datastore_found_response.json");

        let actual: datastore::LookupResult = serde_json::from_str(&body).unwrap();

        let mut properties = HashMap::new();

        properties.insert(
            "token".to_string(),
            datastore::Value::new_string("LvrNooPprYvSiVwyN3VRIARnc05Pte/dtENtlLpWPZ7cC0O"),
        );

        properties.insert(
            "scopes".to_string(),
            datastore::Value::new_array(vec![
                datastore::Value::new_string("email"),
                datastore::Value::new_string("profile"),
            ].into_iter()),
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
