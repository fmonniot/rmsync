use futures::stream::StreamExt as _;
use google_cloud::{datastore, gmail, GcpClient, UserToken};
use log::{debug, warn};
use rmcloud::DocumentId;
use serde::Deserialize;

mod epub;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Error while calling Google Cloud: {0}")]
    Gcp(#[from] google_cloud::Error),

    #[error("Error while calling FanFiction.Net: {0}")]
    FFN(#[from] fanfictionnet::Error),

    #[error("Errors while calling FanFiction.Net: {0:?}")]
    FFNs(Vec<fanfictionnet::Error>),

    #[error("Error while calling reMarkable cloud: {0}")]
    RMCloud(#[from] rmcloud::Error),

    #[error("Error while building an epub file: {0}")]
    Epub(#[from] epub::Error),

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

/// A newtype over a notification history id
// TODO Introduce a gmail::HistoryId(String) and use this one here
// then have a gmail-watch::pubsub::HistoryId(u32) for the notification part (that's really the only
// place where a u32 is used)
#[derive(Debug, Deserialize)]
pub struct HistoryId(u32);

impl HistoryId {
    /// Convert an history id to a string, because most Google APIs will
    /// asks for untyped string unfortunately.
    pub fn to_string(&self) -> String {
        self.0.to_string()
    }
}

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
            .ok_or(Error::MissingDatastoreUserField("token"))?;

        let scopes: Vec<String> = entity
            .properties
            .get("scopes")
            .and_then(|v| v.as_array())
            .map(|a| a.values.iter().flat_map(|v| v.as_string()).collect())
            .ok_or(Error::MissingDatastoreUserField("scopes"))?;

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

    let r = result.as_ref().and_then(|entities| entities.first());

    if let Some(entity) = r {
        let u = DatastoreUser::from_entity(&entity)?;

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

    client
        .cloud_datastore_update_entity(transaction, entity)
        .await?;

    Ok(())
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

pub async fn get_emails<I: Iterator<Item = gmail::MessageId>>(
    client: &GcpClient,
    token: &UserToken,
    message_ids: I,
) -> Result<Vec<EmailMessage>, Error> {
    let results: Vec<_> = futures::stream::iter(message_ids)
        .chunks(100)
        .then(|chunk| client.gmail_get_messages(token, chunk.into_iter()))
        .collect()
        .await;

    // It seems the convertion from Vec<Result to Result<Vec isn't implemented on streams (plus some type guiding)
    let results: Result<Vec<_>, _> = results.into_iter().collect();
    let results = results?;

    let mut emails = Vec::new();
    for message in results.into_iter().flatten() {
        match EmailMessage::from(message) {
            Ok(e) => emails.push(e),
            Err(e) => warn!("Couldn't parse email: {:?}", e),
        };
    }

    Ok(emails)
}

pub async fn upload_ffnet_chapter(
    rm_cloud: &rmcloud::Client,
    story_id: fanfictionnet::StoryId,
    chapter: fanfictionnet::ChapterNum,
) -> Result<(), Error> {
    let chapter = fanfictionnet::fetch_story_chapter(story_id, chapter).await?;

    let file_name = format!("{} - Ch {}.epub", chapter.story_title(), chapter.number());
    let epub = epub::from_chapter(chapter)?;

    // Going blind on this upload. There won't be any conflict because we generate a new
    // document id, but it might produce duplicate epub.
    rm_cloud
        .upload_epub(&epub, &file_name, DocumentId::empty())
        .await?;

    Ok(())
}

pub async fn upload_ffnet_story(
    rm_cloud: &rmcloud::Client,
    story_id: fanfictionnet::StoryId,
) -> Result<(), Error> {
    let chapter_one = fanfictionnet::ChapterNum::new(1);
    let first_chapter = fanfictionnet::fetch_story_chapter(story_id, chapter_one).await?;
    let file_name = format!("{}.epub", first_chapter.story_title());

    debug!("first_chapter:{:?}", first_chapter);

    // Fetch the remaining chapters for this story (if any)
    let mut chapters = if first_chapter.number_of_chapters() < 2 {
        Vec::new()
    } else {
        let range = (2..=first_chapter.number_of_chapters())
            .map(fanfictionnet::ChapterNum::new)
            .into_iter();

        let chapters: Vec<_> = futures::stream::iter(range)
            .map(|c| fanfictionnet::fetch_story_chapter(story_id, c))
            .buffer_unordered(first_chapter.number_of_chapters() as usize)
            .collect()
            .await;

        collect(chapters.into_iter()).map_err(Error::FFNs)?
    };

    // Add the first chapter (the list will be sorted before building the epub)
    chapters.push(first_chapter);

    let epub = epub::from_story(chapters)?;

    // Going blind on this upload. There won't be any conflict because we generate a new
    // document id, but it might produce duplicate epub.
    rm_cloud
        .upload_epub(&epub, &file_name, DocumentId::empty())
        .await?;

    Ok(())
}

fn collect<T, E, I: Iterator<Item = Result<T, E>>>(iter: I) -> Result<Vec<T>, Vec<E>> {
    let mut oks = Vec::new();
    let mut errs = Vec::new();

    for i in iter {
        match i {
            Ok(t) => oks.push(t),
            Err(e) => errs.push(e),
        }
    }

    if errs.is_empty() {
        Ok(oks)
    } else {
        Err(errs)
    }
}
