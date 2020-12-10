//#![deny(warnings)]

use std::convert::Infallible;

use epub_builder::EpubBuilder;
use epub_builder::EpubContent;
use epub_builder::ReferenceType;
use epub_builder::ZipLibrary;
use fanfictionnet::Chapter;
use futures::stream::{StreamExt as _};
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server};
use log::warn;
use log::{debug, error, info};
use regex::Regex;
use rmcloud::DocumentId;
use serde::Deserialize;
use std::sync::Arc;

mod gcp;
mod tokens;

// {"emailAddress": "user@example.com", "historyId": "9876543210"}
#[derive(Debug, Deserialize)]
struct Notification {
    #[serde(rename = "emailAddress")]
    email_address: String,

    #[serde(rename = "historyId")]
    history_id: gcp::HistoryId,
}

#[derive(Debug)]
enum Error {
    FFN(fanfictionnet::Error),
    RMCloud(rmcloud::Error),
    Epub(epub_builder::Error),
    Io(std::io::Error),
    Gcp(gcp::Error),
    Token(tokens::TokenError),
    InvalidEmailContent,
}

impl From<fanfictionnet::Error> for Error {
    fn from(error: fanfictionnet::Error) -> Self {
        Error::FFN(error)
    }
}

impl From<rmcloud::Error> for Error {
    fn from(error: rmcloud::Error) -> Self {
        Error::RMCloud(error)
    }
}

impl From<epub_builder::Error> for Error {
    fn from(error: epub_builder::Error) -> Self {
        Error::Epub(error)
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::Io(error)
    }
}

impl From<gcp::Error> for Error {
    fn from(error: gcp::Error) -> Self {
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

    let result = cfg
        .gcp
        .cloud_datastore_user_by_email(&notification.email_address)
        .await?;

    let mut user_token = match result {
        gcp::DatastoreLookup::Found { token } => {
            tokens::UserToken::from_encrypted_blob(&cfg.crypto, &token)?
        }
        _ => {
            todo!("return an error")
        }
    };

    // Might be skippable within the first hours after login, but otherwise always required
    cfg.gcp.refresh_user_token(&mut user_token).await?;

    let history = cfg
        .gcp
        .gmail_users_history_list(&user_token, notification.history_id)
        .await?;

    info!("Will fetch {} emails", history.len());

    // The Batch APIs can't process more than 100 requests at once, so let's break our queries down if necessary
    let results: Vec<_> = futures::stream::iter(history)
        .chunks(100)
        .then(|chunk| cfg.gcp.gmail_get_messages(&user_token, chunk.into_iter()))
        .collect()
        .await;

    // It seems the convertion from Vec<Result to Result<Vec isn't implemented on streams
    let results: Result<Vec<_>, _> = results.into_iter().collect();

    let emails: Vec<_> = results?
        .into_iter()
        .flatten()
        .filter(|e| &e.from == "FanFiction <bot@fanfiction.com>")
        .collect();

    info!("Found {} FanFiction.Net emails", emails.len());

    if emails.len() <= 0 {
        return Ok(());
    }

    let mut rm_cloud = rmcloud::make_client()?;
    rm_cloud.renew_token().await?;

    for email in emails {
        let content = email.body.ok_or(Error::InvalidEmailContent)?;
        let (story_id, chapter) = parse_ffn_email(&content).ok_or(Error::InvalidEmailContent)?;

        let chapter = fanfictionnet::fetch_story_chapter(story_id, chapter).await?;

        let file_name = format!("{} - Ch {}.epub", chapter.story_title(), chapter.number());
        let epub = make_epub(chapter).await?;

        debug!("uploading epub: {}", file_name);

        // Going blind on this upload. Let's assume there won't be any conflict for now
        // and see how it develops over time.
        rm_cloud
            .upload_epub(&epub, &file_name, DocumentId::empty())
            .await?;
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

async fn make_epub(chapter: Chapter) -> Result<Vec<u8>, Error> {
    let mut buffer = Vec::new();

    let content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
<body>
{}
</body>
</html>"#,
        chapter.content()
    );

    EpubBuilder::new(ZipLibrary::new()?)?
        // Set some metadata
        .metadata("author", chapter.author())?
        .metadata("title", chapter.story_title())?
        .add_content(
            EpubContent::new("chapter_1.xhtml", content.as_bytes())
                .title(chapter.title())
                .reftype(ReferenceType::Text),
        )?
        .generate(&mut buffer)?;

    Ok(buffer)
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
            return Ok(Response::builder()
                .status(400)
                .body(Body::empty())
                .unwrap());
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
    pretty_env_logger::init(); // use env_logger directly and see if colors are supported

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
    gcp: gcp::GcpClient,
    crypto: tokens::Cryptographer,
}

impl Configuration {
    async fn from_env() -> Result<Configuration, Box<dyn std::error::Error + Send + Sync>> {
        let google_client_id = std::env::var("GOOGLE_CLIENT_ID")?;
        let google_client_secret = std::env::var("GOOGLE_CLIENT_SECRET")?;
        let project_id = std::env::var("GCP_PROJECT")?;

        let port: u16 = std::env::var("PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(8080);

        let gcp = gcp::make_client(
            project_id.clone(),
            google_client_id.clone(),
            google_client_secret.clone(),
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
