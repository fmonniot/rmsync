//#![deny(warnings)]

use std::convert::Infallible;

use epub_builder::EpubBuilder;
use epub_builder::EpubContent;
use epub_builder::ReferenceType;
use epub_builder::ZipLibrary;
use fanfictionnet::Chapter;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server};
use log::{error, info};
use rmcloud::DocumentId;
use tokio::prelude::*;

struct Notification;

#[derive(Debug)]
enum Error {
    FFN(fanfictionnet::Error),
    RMCloud(rmcloud::Error),
    Epub(epub_builder::Error),
    Io(std::io::Error),
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

async fn convert(_notification: Notification) -> Result<(), Error> {
    let _ = fetch_mail_content().await?;

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

/// Handle the interface between the HTTP transport and the business functions
async fn http_handler(_: Request<Body>) -> Result<Response<Body>, Infallible> {
    let notificaton: Notification = Notification; // TODO

    match convert(notificaton).await {
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

    // For every connection, we must make a `Service` to handle all
    // incoming HTTP requests on said connection.
    let make_svc = make_service_fn(|_conn| {
        // This is the `Service` that will handle the connection.
        // `service_fn` is a helper to convert a function that
        // returns a Response into a `Service`.
        async { Ok::<_, Infallible>(service_fn(http_handler)) }
    });

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let addr = ([127, 0, 0, 1], port).into();

    let server = Server::bind(&addr).serve(make_svc);

    info!("Listening on http://{}", addr);

    server.await?;

    Ok(())
}
