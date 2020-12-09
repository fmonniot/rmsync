//#![deny(warnings)]

use std::convert::Infallible;

use epub_builder::EpubBuilder;
use epub_builder::EpubContent;
use epub_builder::ReferenceType;
use epub_builder::ZipLibrary;
use fanfictionnet::Chapter;
use futures::stream::{FuturesUnordered, StreamExt as _};
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server};
use log::warn;
use log::{error, info};
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

    let res = cfg.gcp.gmail_get_messages(&user_token, history.into_iter().take(3)).await;

    info!("batch response: {:?}", res);

    // TODO I might not want to make all the calls at once and race them
    // TODO Or I might implement the batching APIs (if I can understand them, geez Google make everything complicated…)
    // Fetch all emails in parallel

    // Start playing with batch api instead of manually calling the API
    /*
    let futures: FuturesUnordered<_> = history
        .iter()
        .map(|h| cfg.gcp.gmail_get_message(h))
        .collect();
    let email_results: Vec<_> = futures.collect().await;

    // Keep the successes and discards the failures (with a warn)
    let mut emails = Vec::new();
    for r in email_results {
        match r {
            Ok(e) => emails.push(e),
            Err(e) => warn!("Couldn't fetch email. Error = {:?}", e),
        }
    }

    println!("fetched emails: {:#?}", emails);
    */

    let _ = fetch_mail_content().await?;

    // While I'm developing the Google Cloud side of things,
    // let's not create resources on the remarkable cloud.
    return Ok(());

    // Will come from the mail content
    let sid = fanfictionnet::new_story_id(4985743);
    let chapter = fanfictionnet::new_chapter_number(38);

    let chapter = fanfictionnet::fetch_story_chapter(sid, chapter).await?;

    let file_name = format!("{} - Ch {}.epub", chapter.story_title(), chapter.number());
    let epub = make_epub(chapter).await?;

    let mut rm_cloud = rmcloud::make_client()?;

    rm_cloud.renew_token().await?;

    rm_cloud
        .upload_epub(&epub, &file_name, DocumentId::empty())
        .await?;

    // TODO Evaluate if listing all documents before upload is necessary, and if it is,
    // how (or if) can I cache this result (speed up, rmcloud usage, gcp costs, etc…)
    //
    let documents = rm_cloud.list_documents().await?;
    let documents = documents
        .iter()
        .map(|d| {
            format!(
                "Document(name:'{}', type:{}, {:?})",
                d.visible_name, d.tpe, d.id
            )
        })
        .collect::<Vec<_>>();
    println!("Documents: {:#?}", documents);

    Ok(())
}

async fn fetch_mail_content() -> Result<(), Error> {
    Ok(())
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
        message_id: String,
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
            let req_id = uuid::Uuid::new_v4().to_string();
            warn!(
                "Can't the read the request body because of error: {:?} (Req-Id: {})",
                error, req_id
            );
            return Ok(Response::builder()
                .status(400)
                .body(format!("{{\"request-id\":\"{}\"}}", req_id).into())
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
    pretty_env_logger::init();

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

    let addr = ([127, 0, 0, 1], configuration.port).into();
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
