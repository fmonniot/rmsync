use crate::tokens::UserToken;
use gcp_auth::Token;
use log::{debug, warn};
use serde::Deserialize;
use serde_json::json;
use mime::Mime;

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

    pub async fn gmail_get_messages<I: Iterator<Item = MessageId>>(&self, token: &UserToken, message_ids: I) -> Result<Vec<EmailMessage>, Error> {
        debug!("Fetching messages in batch");
        use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_LENGTH, CONTENT_TYPE};


        let req = self.http.post("https://www.googleapis.com/batch/gmail/v1")
            .bearer_auth(token.as_str());
        
        let res = multipart::gmail_get_messages_batch(req, message_ids.map(|i| i.0.clone())).send().await?;

        let h = res.headers().get(CONTENT_TYPE).ok_or(Error::MissingValidMultipartContentType)?;

        println!("content-type: {:?}", h);
        let mime = h.to_str().unwrap().parse::<Mime>().unwrap(); // TODO Errors

        
        // Might need convertion, let's see what httparse returns us
        let boundary = mime.get_param("boundary").unwrap().to_string(); // TODO Errors

        // content-type: "multipart/mixed; boundary=batch_47XVjsIfPXGWk00LSpbABytFrk9NfNT3"

        let response_body = res.text().await?;
        
        println!("batch.response.body = {}", response_body);

        multipart::read_multipart_response(&boundary, &response_body);

        Ok(vec![])
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
    from: String,
    //body: String,
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
        data: String,
    }

    impl MessagePartBody {
        pub(super) fn decoded_data(&self) -> Result<String, super::Error> {
            if self.size == 0 {
                Err(base64::DecodeError::InvalidLength)?;
            }

            let bytes = base64::decode(&self.data)?;
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

    use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_LENGTH, CONTENT_TYPE};
    use reqwest::{Client, RequestBuilder, Response};
    use bytes::Bytes;

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
    pub(super) fn gmail_get_messages_batch<I: Iterator<Item = String>>(builder: RequestBuilder, ids: I) -> RequestBuilder {
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

        println!("request.body = {}", String::from_utf8(body.clone()).unwrap());

        builder
            .header(CONTENT_TYPE, "multipart/mixed; boundary=batch_foobarbaz")
            .header(CONTENT_LENGTH, body.len())
            .body(body)
    }


    // we should really do something at the bytes level, looking for known pattern and separating
    // into bytes in the middle
    pub(super) fn read_multipart_response(boundary: &str, body: &String) {

        for (idx, part) in body.split(&format!("--{}", boundary)).enumerate() {
            println!("part size: {}", part.len())
        }

    }

    // Tests, will probably move to a submodule
    use std::path::PathBuf;

    fn asset(p: &str) -> String {
        let mut d = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        d.push("assets");
        d.push(p);

        std::fs::read_to_string(d).unwrap()
    }

    //#[test]
    #[tokio::test]
    async fn test_example_request() {
        let response = asset("multipart_http_response.txt");
        let response = Bytes::from(response);
        
        read_multipart_response(&"batch_foobarbaz", &response);
    }
}

#[derive(Debug, Deserialize)]
pub struct HistoryId(String);

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
