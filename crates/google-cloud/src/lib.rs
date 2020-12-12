use crate::tokens::UserToken;
use gcp_auth::Token;
use log::{debug, warn};
use serde::Deserialize;
use serde_json::json;

pub mod tokens;
pub mod datastore;
pub mod gmail;
mod multipart;

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

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Error while (de)serializing JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Error while calling an API: {0}")]
    Http(#[from] reqwest::Error),
    
    #[error("Error while retrieving auth info: {0}")]
    GcpAuth(#[from] gcp_auth::Error),
    
    #[error("Error while decoding base64 content: {0}")]
    Base64(#[from] base64::DecodeError),
    
    #[error("Not a valid UTF-8 string: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    
    #[error("Error while decoding a multipart response: {0}")]
    DecodeMultipart(#[from] multipart::ReadMultipartError),

    #[error("Missing fields in the datastore entity: {0}")]
    MissingDatastoreUserField(&'static str),

    #[error("Missing `From` header in a message")]
    EmailWithoutFromField,

    #[error("Tried to decode a body without data. This can happens when there are multiple body in a single message (multipart)")]
    NoBodyToDecode,
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
                Some(result) => Ok(DatastoreLookup::Found(DatastoreUser::from_entity(
                    &result.entity,
                )?)),
            },
            datastore::LookupResult::Missing { .. } => Ok(DatastoreLookup::Missing),
        }
    }

    pub async fn cloud_datastore_update_history_id(
        &self,
        email: &str,
        user: &DatastoreUser,
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
        history_id: &HistoryId,
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

// TODO Recipes
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
            .and_then(|v| v.as_string())
            .ok_or(Error::MissingDatastoreUserField(
                "token"
            ))?;

        let scopes: Vec<String> = entity
            .properties
            .get("scopes")
            .and_then(|v| v.as_array())
            .map(|a| a.values.iter().flat_map(|v| v.as_string()).collect())
            .ok_or(Error::MissingDatastoreUserField(
                "scopes"
            ))?;

        let last_known_history_id = entity.properties.get("history_id").and_then(|v| v.as_u32());

        Ok(DatastoreUser {
            token,
            scopes,
            last_known_history_id,
        })
    }

    fn as_entity(&self, key: datastore::Key) -> datastore::Entity {
        let mut properties = std::collections::HashMap::new();

        properties.insert(
            "token".to_string(),
            datastore::Value::new_string(&self.token, Some(true)),
        );
        properties.insert(
            "scopes".to_string(),
            datastore::Value::new_array(
                self.scopes
                    .iter()
                    .map(|s| datastore::Value::new_string(s, Some(true))),
            ),
        );
        if let Some(id) = self.last_known_history_id {
            properties.insert(
                "history_id".to_string(),
                datastore::Value::new_integer(id, Some(true)),
            );
        }

        datastore::Entity { key, properties }
    }

    pub fn new_history(&mut self, history_id: &HistoryId) {
        self.last_known_history_id.replace(history_id.0);
    }
}

// TODO Recipes
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

#[derive(Debug, Deserialize)]
pub struct HistoryId(pub u32);

#[derive(Debug, Deserialize, PartialEq)]
pub struct MessageId(String);

