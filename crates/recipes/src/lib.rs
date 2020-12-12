
use google_cloud::{GcpClient, datastore};
use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Error while calling Google Cloud: {0}")]
    Gcp(#[from] google_cloud::Error),

    //#[error("Error while (de)serializing JSON: {0}")]
    //Json(#[from] serde_json::Error),

    //#[error("Error while decoding base64 content: {0}")]
    //Base64(#[from] base64::DecodeError),
    
    #[error("Not a valid UTF-8 string: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    
    #[error("Missing fields in the datastore entity: {0}")]
    MissingDatastoreUserField(&'static str),

    #[error("Missing `From` header in a message")]
    EmailWithoutFromField,

    #[error("Tried to decode a body without data. This can happens when there are multiple body in a single message (multipart)")]
    NoBodyToDecode,
}



// TODO Remove .0 public access once gmail helpers have been moved here
#[derive(Debug, Deserialize)]
pub struct HistoryId(pub u32);


/// A representation of a user in Cloud Datastore
#[derive(Debug)]
pub struct DatastoreUser {
    pub token: String,
    scopes: Vec<String>,
    last_known_history_id: Option<u32>,
}

impl DatastoreUser {
    fn key(email: &str, project_id: &str) -> datastore::Key {
        datastore::Key {
            partition_id: datastore::PartitionId {
                namespace: None,
                project_id: project_id.to_string(),
            },
            path: vec![datastore::PathElement {
                kind: "oauth2token".to_string(),
                name: email.to_string(),
            }],
        }
    }

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


pub async fn fetch_user_by_email(
    client: &GcpClient,
    email: &str,
) -> Result<Option<DatastoreUser>, Error> {
    let key = DatastoreUser::key(email, &client.project_id());

    let result = client.cloud_datastore_lookup(vec![key]).await?;

    let r = result
        .as_ref()
        .and_then(|entities| entities.first());

    if let Some(entity) = r {
        let u = DatastoreUser::from_entity(
            &entity,
        )?;

        Ok(Some(u))
    } else {
        Ok(None)
    }
}

pub async fn update_user(
    client: &GcpClient,
    email: &str,
    user: &DatastoreUser,
) -> Result<(), Error> {

    let transaction = client.cloud_datastore_begin_transaction().await?;

    let key = DatastoreUser::key(email, &client.project_id());
    let entity = user.as_entity(key);

    client.cloud_datastore_update_entity(transaction, entity).await?;

    Ok(())
}
