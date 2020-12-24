//#![deny(warnings)]

use std::convert::Infallible;

use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server};
use log::warn;
use log::{debug, error, info};
use regex::Regex;
use serde::Deserialize;
use std::sync::Arc;

use google_cloud::{tokens, Error as GcpError, GcpClient};
use recipes::HistoryId;

// {"emailAddress": "user@example.com", "historyId": "9876543210"}
#[derive(Debug, Deserialize)]
struct Notification {
    #[serde(rename = "emailAddress")]
    email_address: String,

    #[serde(rename = "historyId")]
    history_id: HistoryId,
}

#[derive(Debug)]
enum Error {
    RMCloud(rmcloud::Error),
    Io(std::io::Error),
    Gcp(GcpError),
    Recipes(recipes::Error),
    Token(tokens::TokenError),
    InvalidEmailContent,
}

impl From<rmcloud::Error> for Error {
    fn from(error: rmcloud::Error) -> Self {
        Error::RMCloud(error)
    }
}

impl From<recipes::Error> for Error {
    fn from(error: recipes::Error) -> Self {
        Error::Recipes(error)
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::Io(error)
    }
}

impl From<GcpError> for Error {
    fn from(error: GcpError) -> Self {
        Error::Gcp(error)
    }
}

impl From<tokens::TokenError> for Error {
    fn from(error: tokens::TokenError) -> Self {
        Error::Token(error)
    }
}

// TODO Find a way to create clients only once. Thinking of GCP and Crypto, maybe rmcloud.
// The idea being if many emails arrive at once, we can reuse the tokens across sessions
// instead of creating new one every time.
// Definitively an optimization though.

async fn convert(notification: Notification, cfg: Arc<Configuration>) -> Result<(), Error> {
    info!("Received notification {:?}", notification);

    // Create a distributed lock on the notification email. This is to avoid concurrent processing
    // when receiving a burst of notifications.
    let lock = cfg
        .gcp
        .cloud_storage_new_lock(notification.email_address.clone());
    let lock_acquired = lock.lock().await?;

    if !lock_acquired {
        // After trying for 6 seconds, we couldn't acquire the lock. Let's stop here
        warn!(
            "Couldn't acquire lock on account {}. Stoping here.",
            notification.email_address
        );
        return Ok(());
    } else {
        debug!("Lock acquired. Continuing execution.");
    }

    let result = recipes::fetch_user_by_email(&cfg.gcp, &notification.email_address).await?;

    let mut user = match result {
        Some(user) => user,
        _ => {
            todo!("return an error")
        }
    };

    let mut user_token = tokens::UserToken::from_encrypted_blob(&cfg.crypto, &user.token)?;
    let history_id = user.history_id().unwrap_or_else(|| notification.history_id);

    // Might be skippable within the first hours after login, but otherwise always required
    cfg.gcp.identity_refresh_user_token(&mut user_token).await?;

    let history = cfg
        .gcp
        .gmail_users_history_list(&user_token, &history_id.to_string())
        .await?;

    info!("Will fetch {} emails", history.len());

    let emails = recipes::get_emails(&cfg.gcp, &user_token, history.into_iter()).await?;

    let emails: Vec<_> = emails
        .into_iter()
        .filter(|e| &e.from == "FanFiction <bot@fanfiction.com>")
        .collect();

    info!("Found {} FanFiction.Net emails", emails.len());

    // Only interact with the remarkable cloud if we are going to upload some documents
    if emails.len() > 0 {
        let mut rm_cloud = rmcloud::make_client()?;
        rm_cloud.renew_token().await?;

        for email in emails {
            let content = email.body.ok_or(Error::InvalidEmailContent)?;
            let (story_id, chapter) =
                parse_ffn_email(&content).ok_or(Error::InvalidEmailContent)?;

            recipes::upload_ffnet_chapter(&rm_cloud, story_id, chapter).await?;
        }
    }

    // Update our database with the current history id, to look up on next invokation
    let new_history_id = &notification.history_id;
    if user.is_history_more_recent(new_history_id) {
        debug!(
            "User's history_id ({:?}) will be updated to ({:?})",
            user.history_id(),
            new_history_id
        );
        user.new_history(new_history_id);
        recipes::update_user(&cfg.gcp, &notification.email_address, &user).await?;
    } else {
        debug!(
            "User's history_id ({:?}) will NOT be updated to ({:?})",
            user.history_id(),
            new_history_id
        );
    }

    if !lock.unlock().await? {
        warn!("Couldn't unlock email. Look up logs to see if another function did it concurrently.")
    }

    Ok(())
}

fn parse_ffn_email(content: &str) -> Option<(fanfictionnet::StoryId, fanfictionnet::ChapterNum)> {
    lazy_static::lazy_static! {
        static ref RE: Regex = Regex::new("fanfiction\\.net/s/(\\d+)/(\\d+)/").unwrap();
    }

    RE.captures(content).map(|cap| {
        // The captures matched the regex, so they are numbers. Still, the convertion can panic
        // if the integers are too big for their containers.
        let id = cap[1].parse::<u32>().unwrap();
        let ch = cap[2].parse::<u16>().unwrap();

        (
            fanfictionnet::new_story_id(id),
            fanfictionnet::new_chapter_number(ch),
        )
    })
}

/// Google Pub/Sub will wrap the actual message within some metadata information.
/// This module offer a simple `deserialize` method to extract the actual payload
/// from the metadata.
///
///
/// Example of a message received via Pub/Sub, notice how the actual content is
/// within the `data` field :
///    ```json
///    {
///        message:
///        {
///          // This is the actual notification data, as base64url-encoded JSON.
///          data: "eyJlbWFpbEFkZHJlc3MiOiAidXNlckBleGFtcGxlLmNvbSIsICJoaXN0b3J5SWQiOiAiMTIzNDU2Nzg5MCJ9",
///      
///          // This is a Cloud Pub/Sub message id, unrelated to Gmail messages.
///          message_id: "1234567890",
///        }
///      
///        subscription: "projects/myproject/subscriptions/mysubscription"
///      }
///    ```
///
/// https://developers.google.com/gmail/api/guides/push?hl=en
mod pubsub {
    use bytes::buf::BufExt as _;
    use bytes::Buf;
    use serde::{de::DeserializeOwned, Deserialize};

    #[derive(Debug)]
    pub enum Error {
        Json(serde_json::Error),
        Base64(base64::DecodeError),
    }

    impl From<serde_json::Error> for Error {
        fn from(error: serde_json::Error) -> Self {
            Error::Json(error)
        }
    }

    impl From<base64::DecodeError> for Error {
        fn from(error: base64::DecodeError) -> Self {
            Error::Base64(error)
        }
    }

    #[derive(Debug, Deserialize)]
    pub struct Envelope {
        message: Message,
        subscription: String,
    }

    #[derive(Debug, Deserialize)]
    pub struct Message {
        data: String,
    }

    pub fn deserialize<T: DeserializeOwned, B: Buf>(buf: B) -> Result<T, Error> {
        let envelope: Envelope = serde_json::from_reader(buf.reader())?;
        let data = base64::decode(envelope.message.data)?;

        Ok(serde_json::from_slice(&data)?)
    }
}

/// Handle the interface between the HTTP transport and the business functions
async fn http_handler(
    req: Request<Body>,
    cfg: Arc<Configuration>,
) -> Result<Response<Body>, Infallible> {
    // Read the body as a JSON notification
    let whole_body = hyper::body::aggregate(req)
        .await
        .expect("Can't read request body");

    let notification = match pubsub::deserialize(whole_body) {
        Ok(data) => data,
        Err(error) => {
            warn!(
                "Can't the read the request body because of error: {:?}",
                error
            );
            return Ok(Response::builder().status(400).body(Body::empty()).unwrap());
        }
    };

    match convert(notification, cfg).await {
        Ok(()) => Ok(Response::builder().status(200).body(Body::empty()).unwrap()),
        Err(e) => {
            error!("Error while handling email: {:?}", e);
            Ok(Response::builder().status(500).body(Body::empty()).unwrap())
        }
    }
}

#[tokio::main]
pub async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    env_logger::init();

    let configuration = Arc::new(Configuration::from_env().await?);

    // For every connection, we must make a `Service` to handle all
    // incoming HTTP requests on said connection.
    let make_svc = make_service_fn(|_conn| {
        let configuration = Arc::clone(&configuration);

        // This is the `Service` that will handle the connection.
        // `service_fn` is a helper to convert a function that
        // returns a Response into a `Service`.
        async move {
            Ok::<_, Infallible>(service_fn(move |req| {
                let configuration = Arc::clone(&configuration);

                http_handler(req, configuration)
            }))
        }
    });

    let addr = ([0, 0, 0, 0], configuration.port).into();
    let server = Server::bind(&addr).serve(make_svc);

    info!("Listening on http://{}", addr);

    server.await?;

    Ok(())
}

struct Configuration {
    port: u16,
    gcp: GcpClient,
    crypto: tokens::Cryptographer,
}

impl Configuration {
    async fn from_env() -> Result<Configuration, Box<dyn std::error::Error + Send + Sync>> {
        let google_client_id = std::env::var("GOOGLE_CLIENT_ID")?;
        let google_client_secret = std::env::var("GOOGLE_CLIENT_SECRET")?;
        let project_id = std::env::var("GCP_PROJECT")?;
        let bucket_name = std::env::var("LOCK_BUCKET_NAME")?;

        let port: u16 = std::env::var("PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(8080);

        let gcp = google_cloud::make_client(
            project_id,
            google_client_id,
            google_client_secret.clone(),
            bucket_name,
        )
        .await?;
        let crypto = tokens::Cryptographer::new(&google_client_secret)?;

        Ok(Configuration { port, gcp, crypto })
    }
}

#[cfg(test)]
mod tests {

    use super::parse_ffn_email;
    use fanfictionnet::{new_chapter_number, new_story_id};

    #[test]
    fn correctly_parse_email() {
        let content = "New chapter from AppoApples,\r\n\r\nSignificant Brain Damage\r\nChapter 31: The Twins of Alderaan\r\n\r\nhttps://www.fanfiction.net/s/13587604/31/Significant-Brain-Damage\r\n\r\nStar Wars\r\n\r\nWords: 3,479\r\nGenre: Drama/Humor\r\nRated: T\r\nCharacter: Luke S., Obi-Wan K., Captain Rex, Ahsoka T.\r\n\r\nSummary: Luke Skywalker finds himself in the past as Anakin Skywalker. Obi-Wan finds himself retraining his old apprentice who has permanent amnesia while also taking on Anakin\'s Padawan, being a General, a Council member -during a Galactic Civil War, and fighting for a Republic he\'s beginning to lose faith in. Clone Wars, no slash, no paradox, no easy fix it.\r\n\r\nFanFiction https://www.fanfiction.net\r\n\r\nFollow us on twitter @ https://twitter.com/fictionpress\r\n\r\n";
        let expected = (new_story_id(13587604), new_chapter_number(31));

        assert_eq!(parse_ffn_email(content), Some(expected))
    }
}
