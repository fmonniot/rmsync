#![deny(warnings)]

use std::convert::Infallible;

use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server};
use log::info;

struct Notification;

enum Error {
    FFN(fanfictionnet::Error),
    RMCloud(rmcloud::Error),
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

async fn convert(_notification: Notification) -> Result<(), Error> {
    let _ = fetch_mail_content().await?;

    // Will come from the mail content
    let sid = fanfictionnet::new_story_id(1);
    let chapter = fanfictionnet::new_chapter_number(1);

    let _chapter = fanfictionnet::fetch_story_chapter(sid, chapter).await?;

    let _epub = make_epub().await?;

    let rm_cloud = rmcloud::make_client()?;

    rm_cloud.upload().await?;

    Ok(())
}

async fn fetch_mail_content() -> Result<(), Error> {
    Ok(())
}

async fn make_epub() -> Result<(), Error> {
    Ok(())
}

/// Handle the interface between the HTTP transport and the business functions
async fn http_handler(_: Request<Body>) -> Result<Response<Body>, Infallible> {
    let notificaton: Notification = Notification; // TODO

    match convert(notificaton).await {
        Ok(()) => Ok(Response::builder().status(200).body(Body::empty()).unwrap()),
        Err(_) => Ok(Response::builder().status(500).body(Body::empty()).unwrap()),
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
