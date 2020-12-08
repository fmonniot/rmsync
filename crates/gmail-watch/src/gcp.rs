use gcp_auth::{AuthenticationManager, Token};
use serde::Deserialize;
use serde_json::json;
use crate::tokens::UserToken;

pub async fn make_client(project_id: String) -> Result<GcpClient, Error> {
    let authentication_manager = gcp_auth::init().await?;
    let token = authentication_manager
        .get_token(&["https://www.googleapis.com/auth/datastore"])
        .await?;
    let http = reqwest::Client::new();

    Ok(GcpClient {
        project_id,
        authentication_manager,
        token,
        http,
    })
}

#[derive(Debug)]
pub enum Error {
    Json(serde_json::Error),
    Http(reqwest::Error),
    GcpAuth(gcp_auth::Error),
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

pub struct GcpClient {
    project_id: String,
    authentication_manager: AuthenticationManager,
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

        // TODO deserialization is gonna be a bit different :)
        Ok(res.json().await?)
    }

    pub async fn gmail_users_history_list(&self, token: &UserToken, history_id: HistoryId) {

    }
}

#[derive(Debug, Deserialize)]
pub enum DatastoreLookup {
    // For now we only return the token
    Found { token: String },
    Missing,
}

#[derive(Debug, Deserialize)]
struct HistoryId(String);
