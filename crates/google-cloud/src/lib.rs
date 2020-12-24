use gcp_auth::Token;
use log::{debug, trace, warn};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde_json::json;

pub use crate::tokens::UserToken;
pub mod datastore;
pub mod gmail;
mod multipart;
pub mod tokens;

// TODO It'd be nice to share the AuthenticationManager and reqwest::Client between connections
pub async fn make_client(
    project_id: String,
    client_id: String,
    client_secret: String,
    lock_bucket: String,
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
        lock_bucket,
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

    #[error("A call to {api:?} failed with status {status} (body: |{body}|)")]
    UnexpectedStatus {
        status: StatusCode,
        body: String,
        api: ApiKind,
    },

    #[error("Missing fields in the datastore entity: {0}")]
    MissingDatastoreUserField(&'static str),

    #[error("Missing `From` header in a message")]
    EmailWithoutFromField,

    #[error("Tried to decode a body without data. This can happens when there are multiple body in a single message (multipart)")]
    NoBodyToDecode,

    #[error("Batch can contains at most 100 requests, {0} asked")]
    BatchTooManyRequests(u16),
}

#[derive(Debug)]
pub enum ApiKind {
    DatastoreLookup,
    BeginTransaction,
    RefreshToken,
    GetHistoryList,

    StorageCreate,
    StorageDelete,
}

pub struct GcpClient {
    project_id: String,
    client_id: String,
    client_secret: String,
    lock_bucket: String,
    token: Token,
    http: reqwest::Client,
}

impl GcpClient {
    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    // https://cloud.google.com/datastore/docs/reference/data/rest/v1/projects/lookup
    pub async fn cloud_datastore_lookup(
        &self,
        keys: Vec<datastore::Key>,
    ) -> Result<Option<Vec<datastore::Entity>>, Error> {
        let json = json!({ "keys": keys });

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

        let result: datastore::LookupResult =
            decode_response(res, ApiKind::DatastoreLookup).await?;

        Ok(result
            .as_option()
            .map(|vec| vec.into_iter().map(|r| r.entity).collect()))
    }

    // https://cloud.google.com/datastore/docs/reference/data/rest/v1/projects/beginTransaction#TransactionOptions
    pub async fn cloud_datastore_begin_transaction(
        &self,
    ) -> Result<datastore::TransactionId, Error> {
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

        let body: datastore::BeginTransactionResponse =
            decode_response(response, ApiKind::BeginTransaction).await?;

        Ok(body.transaction)
    }

    pub async fn cloud_datastore_update_entity(
        &self,
        transaction: datastore::TransactionId,
        entity: datastore::Entity,
    ) -> Result<(), Error> {
        let req = json!({
            "transaction": transaction,
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

        trace!("commit: status:{}, body:|{:?}|", status, body);

        Ok(())
    }

    // https://cloud.google.com/identity-platform/docs/use-rest-api#section-refresh-token
    pub async fn identity_refresh_user_token(&self, token: &mut UserToken) -> Result<(), Error> {
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

        let new_token: identity::RefreshTokenResponse =
            decode_response(res, ApiKind::RefreshToken).await?;

        token.set_access_token(new_token.access_token);

        Ok(())
    }

    // https://developers.google.com/gmail/api/reference/rest/v1/users.history/list
    pub async fn gmail_users_history_list(
        &self,
        token: &UserToken,
        history_id: &str,
    ) -> Result<Vec<gmail::MessageId>, Error> {
        debug!("Fetching user history list (history_id: {})", history_id);

        let res = self
            .http
            .get("https://gmail.googleapis.com/gmail/v1/users/me/history")
            .bearer_auth(&token.as_str())
            .query(&[("startHistoryId", history_id)])
            .send()
            .await?;

        let result: gmail::HistoryListResponse =
            decode_response(res, ApiKind::GetHistoryList).await?;

        // Find out if this history id match the one in the notification
        debug!(
            "gmail.users.history.list return history id: {}",
            result.history_id
        );

        Ok(result
            .history
            .into_iter()
            .flat_map(|h| h.messages)
            .map(|h| h.id)
            .collect())
    }

    // https://developers.google.com/gmail/api/reference/rest/v1/users.messages/get
    // https://developers.google.com/gmail/api/guides/batch
    // TODO For now we ignore all errors. Instead we should return Result<Vec<Result<Message, Error>>, Error>
    pub async fn gmail_get_messages<I: Iterator<Item = gmail::MessageId>>(
        &self,
        token: &UserToken,
        message_ids: I,
    ) -> Result<Vec<gmail::Message>, Error> {
        debug!("Fetching messages in batch");

        let request = self
            .http
            .post("https://www.googleapis.com/batch/gmail/v1")
            .bearer_auth(token.as_str());

        let mut count = 0;

        let request = multipart::gmail_get_messages_batch(
            request,
            message_ids.map(|i| {
                count += 1;
                i.0.clone()
            }),
        );

        // Fail early because we know Google will reject those
        if count > 100 {
            return Err(Error::BatchTooManyRequests(count));
        }

        let response = request.send().await?;

        let responses: Vec<Result<gmail::Message, Error>> = multipart::read_response(response)
            .await?
            .into_iter()
            .map(|r| {
                let r = r?;
                let m = serde_json::from_slice(r.body())?;

                Ok(m)
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

    pub fn cloud_storage_new_lock(&self, object: String) -> Lock {
        Lock {
            bucket: self.lock_bucket.clone(),
            object,
            client: self.http.clone(),
            token: self.token.clone(),
        }
    }
}

async fn decode_response<T: DeserializeOwned>(
    response: reqwest::Response,
    api: ApiKind,
) -> Result<T, Error> {
    debug!("gcp.response.api: {:?}, status: {}", api, response.status());
    let status = response.status();

    if status.is_success() {
        Ok(response.json().await?)
    } else {
        let body = response.text().await?;

        Err(Error::UnexpectedStatus { status, body, api })
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

/// An asynchronous lock system based on Google Cloud Storage.
///
/// It probably is not 100% robust, but it should do the trick
/// in rmsync context.
pub struct Lock {
    bucket: String,
    object: String,
    client: reqwest::Client,
    token: Token,
}

const STORAGE_LOCK_URL: &str = "https://storage.googleapis.com/upload/storage/v1";
const STORAGE_UNLOCK_URL: &str = "https://storage.googleapis.com/storage/v1";

impl Lock {
    // Currently we use the bucket only for locks, so we have a lifecycle configuration set on any objects age (delete after 1 day)
    // to avoid locking an email ad vitam eternam.
    // If we want to use the bucket for more things, we will need to find an alternative to the lifecycle
    // (a) we can use custom time instead of age, and don't set the custom time on non-lock objects (needs to be proven)
    // (b) we can embedded the time within the lock, and delete if too long (needs to think about data races in that case)
    // https://cloud.google.com/storage/docs/json_api/v1/objects/insert
    pub async fn lock(&self) -> Result<bool, Error> {
        let url = format!("{}/b/{}/o", STORAGE_LOCK_URL, self.bucket);

        // query explanation
        // media - Simple upload. Upload the object data only, without any metadata.
        // Makes the operation conditional on whether the object's current generation matches the given value. Setting to 0 makes the operation succeed only if there are no live versions of the object.
        let req = self
            .client
            .post(&url)
            .query(&[
                ("name", self.object.as_str()),
                ("uploadType", "media"),
                ("ifGenerationMatch", "0"),
            ])
            .header("content-type", "text/plain")
            .body("1");

        let acquired = self.send(req, ApiKind::StorageCreate).await?;

        Ok(acquired)
    }

    // https://cloud.google.com/storage/docs/json_api/v1/objects/delete
    pub async fn unlock(&self) -> Result<bool, Error> {
        let object = urlencoding::encode(&self.object);
        let url = format!("{}/b/{}/o/{}", STORAGE_UNLOCK_URL, self.bucket, object);

        let req = self.client.delete(&url);

        Ok(self.send(req, ApiKind::StorageDelete).await?)
    }

    // return whether the request succeed within the retry limit
    async fn send(&self, req: reqwest::RequestBuilder, api: ApiKind) -> Result<bool, Error> {
        let req = std::sync::Arc::new(req.bearer_auth(self.token.as_str()).build()?);

        let duration = tokio::time::Duration::new(1, 0);
        let mut counter = 1;

        // With 3 tries, we will wait a maximum of 1+2+3= 6 seconds
        while counter < 4 {
            // We know we can clone the request, because we can guarantee the body aren't streams
            let response = self.client.execute(req.try_clone().unwrap()).await?;

            debug!("gcp.lock: {:?}, status: {}", api, response.status());
            let status = response.status();

            // Only log body on non-expected status code
            match status {
                StatusCode::OK => return Ok(true),         // lock created
                StatusCode::NO_CONTENT => return Ok(true), // lock deleted
                StatusCode::NOT_FOUND => return Ok(false), // no lock to delete
                StatusCode::PRECONDITION_FAILED => (),     // lock created by another
                _ => {
                    let body = response.text().await?;
                    debug!("gcp.lock.body: {}", body.replace('\n', ""));
                }
            };

            // Not successful lock, wait some time and retry
            tokio::time::delay_for(duration * counter).await;
            counter += 1;
        }

        Ok(false)
    }
}
