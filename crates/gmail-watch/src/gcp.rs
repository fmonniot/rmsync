use crate::tokens::UserToken;
use gcp_auth::{AuthenticationManager, Token};
use serde::Deserialize;
use serde_json::json;

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

    InvalidDatastoreContent(String),
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

    pub async fn gmail_users_history_list(&self, token: &UserToken, history_id: HistoryId) {}
}

#[derive(Debug, Deserialize)]
pub enum DatastoreLookup {
    // For now we only return the token
    Found { token: String },
    Missing,
}

/// data structure used on the wire
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

#[derive(Debug, Deserialize)]
pub struct HistoryId(String);

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
