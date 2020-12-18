//! A trim down implemenation of `multipart/` request and responses.
//!
//! It handles enough to make batch request to GMail. It is a non-goal
//! to handle all the complexity of the multipart specification.

use bytes::{Buf, Bytes};
use httparse::{parse_headers, EMPTY_HEADER};
use log::{debug, error, trace};
use mime::Mime;
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE};
use reqwest::RequestBuilder;

// Because implementing a generic http encoder is way too much rust (the one in hyper
// is 100+ lines long and manage a lot of edge cases). Given that all I need is to include
// the snippet
// ```txt
// --batch_foobarbaz
// Content-Type: application/http
// Content-ID: <item1:12930812@barnyard.example.com>
//
// GET /farm/v1/animals/pony
//
// ...
// ```
// I'm just going to do it manually. It would be helpful if in the future there would
// be a `httpencode` crate, like there is a `httparse` one
pub(super) fn gmail_get_messages_batch<I: Iterator<Item = String>>(
    builder: RequestBuilder,
    ids: I,
) -> RequestBuilder {
    const BOUNDARY: &str = "batch_foobarbaz";
    let mut body = Vec::new();

    for id in ids {
        let string = format!(
            "--{boundary}\n\
              Content-Type: application/http\n\
              Content-Id: {id}\n\
              \n\
              GET /gmail/v1/users/me/messages/{id}\n\
              \n",
            boundary = BOUNDARY,
            id = id,
        );
        body.extend(string.as_bytes());
    }

    body.extend(format!("--{}--", BOUNDARY).as_bytes());

    builder
        .header(CONTENT_TYPE, "multipart/mixed; boundary=batch_foobarbaz")
        .header(CONTENT_LENGTH, body.len())
        .body(body)
}

#[derive(Debug, thiserror::Error)]
pub enum ReadMultipartError {
    #[error("Can't parse HTTP headers: {0}")]
    Httparse(#[from] httparse::Error),

    #[error("HTTP malformed error: {0}")]
    Http(#[from] hyper::http::Error),

    #[error("Can't parse the MIME type: {0}")]
    Mime(#[from] mime::FromStrError),

    #[error("Can't read the header value as a string: {0}")]
    ReadHeaderValue(#[from] hyper::http::header::ToStrError),

    #[error("Can't read the HTTP response: {0}")]
    Reqwest(#[from] reqwest::Error),

    #[error("No status code was found in the response")]
    NoStatusCodeDefined,

    #[error("Partial header on a finite body, something went wrong (in {0})")]
    PartialHeaders(&'static str),

    #[error("The received response doesn't have a Content-Type header")]
    MissingContentType,

    #[error("The received response doesn't have a boundary delimiter")]
    NoBoundary,

    #[error("The part doesn't have the expected application/html")]
    InvalidPartContentType,
}

type ReadResult<T> = Result<T, ReadMultipartError>;

/// Given a response, and only if it has a `multipart/mixed` content type
/// with a boundary set, parse the response body and return the individual
/// responses (as hyper [`Response`](hyper::Response) not reqwest [`Response`](reqwest::Response)).
pub(super) async fn read_response(
    response: reqwest::Response,
) -> ReadResult<Vec<ReadResult<hyper::Response<Bytes>>>> {
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .ok_or(ReadMultipartError::MissingContentType)?
        .to_str()?
        .parse::<Mime>()?;

    // Extract the boundary separator from the Content-Type
    let boundary = content_type
        .get_param("boundary")
        .ok_or(ReadMultipartError::NoBoundary)?
        .to_string();

    let response_body = response.bytes().await?;

    debug!("multipart.response.boundary = {}", boundary);

    Ok(read_response_body(&boundary, response_body))
}

/// Given a `multipart/mixed` response body, parse each part of the response
/// and return each response as hyper's [`Response<Bytes>`](hyper::Response).
pub(super) fn read_response_body(
    boundary: &str,
    body: Bytes,
) -> Vec<ReadResult<hyper::Response<Bytes>>> {
    MultipartReader::new(body, boundary)
        .map(parse_multipart_response)
        .collect()
}

/// Given a part of a `multipart/mixed` body, parse the part headers and its body
/// if the `Content-Type` is `application/html`.
fn parse_multipart_response(mut raw_response: Bytes) -> ReadResult<hyper::Response<Bytes>> {
    // There should only be Content-Type and Content-Length as the part headers
    const HEADER_LEN: usize = 2;
    const CONTENT_TYPE: &str = "Content-Type";
    const APPLICATION_HTTP: &[u8] = "application/http".as_bytes();

    // We just want to find out the end of the part headers, so we read them using
    // httparse::parse_headers and discard the result (we know there are only two of them)
    let mut raw_headers = [EMPTY_HEADER; HEADER_LEN];
    match parse_headers(&raw_response, &mut raw_headers)? {
        httparse::Status::Partial => {
            return Err(ReadMultipartError::PartialHeaders("part headers"));
        }
        httparse::Status::Complete((end_headers, headers)) => {
            if headers
                .iter()
                .find(|h| h.name == CONTENT_TYPE && h.value == APPLICATION_HTTP)
                .is_none()
            {
                // No "Content-Type: application/html" header found, let's bail early
                return Err(ReadMultipartError::InvalidPartContentType);
            }

            raw_response.advance(end_headers);

            let (builder, body_position) = parse_http_response(raw_response.clone())?;

            raw_response.advance(body_position);

            let b = builder.body(raw_response)?;

            return Ok(b);
        }
    }
}

/// Parse the status-line and headers of a HTTP response returning a hyper's builder
/// populated with the parsed information as well as the position within the `Bytes`
/// where the response body start.
fn parse_http_response(raw_response: Bytes) -> ReadResult<(hyper::http::response::Builder, usize)> {
    let r: &[u8] = &raw_response;
    let mut h = [httparse::EMPTY_HEADER; 10];
    let mut response = httparse::Response::new(&mut h);

    let body_position = match response.parse(r)? {
        httparse::Status::Partial => {
            return Err(ReadMultipartError::PartialHeaders("inner response"))
        }
        httparse::Status::Complete(p) => p,
    };

    let status = response
        .code
        .ok_or(ReadMultipartError::NoStatusCodeDefined)?;
    let mut builder = hyper::Response::builder().status(status);

    for h in response.headers {
        builder = builder.header(h.name, h.value);
    }

    Ok((builder, body_position))
}

struct MultipartReader {
    bytes: Bytes,
    position: usize,
    boundary: Bytes,
    boundary_end: Bytes,
}

const BOUNDARY_SEP: &[u8] = "--".as_bytes();
const NEWLINE: &[u8] = "\r\n".as_bytes(); // HTTP use CRLF as newline character

impl MultipartReader {
    fn new(bytes: Bytes, boundary: &str) -> MultipartReader {
        let boundary_bytes = boundary.as_bytes();
        let start = [NEWLINE, BOUNDARY_SEP, boundary_bytes, NEWLINE].concat();
        let boundary = Bytes::copy_from_slice(&start);
        let end = [NEWLINE, BOUNDARY_SEP, boundary_bytes, BOUNDARY_SEP, NEWLINE].concat();
        let boundary_end = Bytes::copy_from_slice(&end);

        MultipartReader {
            bytes,
            position: 0,
            boundary,
            boundary_end,
        }
    }

    // The whole algorimth assume we have the data in memory and try to not copy said data.
    // We might want to use a stream of data instead, let's see how it goes.
    fn next_part(&mut self) -> Option<Bytes> {
        // This clone will only clone the pointer to the underlying memory, not the data itself
        let mut bytes = self.bytes.clone();

        // Let's resume where we left off on the last iteration
        bytes.advance(self.position);

        trace!(
            "next: self.position: {}; remaining: {}",
            self.position,
            bytes.remaining()
        );

        // Nothing remains in the buffer so we are done looking into the multipart content
        if !bytes.has_remaining() {
            return None;
        }

        // We have to keep track of the position within the Buf ourselves
        let mut position = self.position;
        let mut boundary_found = false;
        let mut boundary_offset = 0;

        loop {
            // We are pointing at the start of a boundary
            if bytes.remaining() >= self.boundary.len()
                && &bytes[0..self.boundary.len()] == self.boundary
            {
                boundary_found = true;
                boundary_offset = self.boundary.len();
                break;
            }

            // We are at the end of the body, looking for the ending boundary
            if bytes.remaining() == self.boundary_end.len()
                && &bytes[0..self.boundary_end.len()] == self.boundary_end
            {
                boundary_found = true;
                boundary_offset = self.boundary_end.len();
                break;
            }

            // stop gap if no boundary have been found and the body is all read
            if !bytes.has_remaining() {
                break;
            }

            bytes.advance(1);
            position += 1;
        }

        trace!(
            "after loop: position: {}; boundary_found: {}; boundary_offset: {}",
            position, boundary_found, boundary_offset
        );

        if boundary_found {
            if position == 0 {
                // special case, on the very first loop the boundary is found first so we have no
                // part to return yet. So we advance the buffer by the boundary and do another run.
                // This is to avoid returning an empty Bytes on the first call.
                self.position += boundary_offset;
                return self.next();
            }

            let slice = self.bytes.slice(self.position..position);
            self.position = position + boundary_offset;

            Some(slice)
        } else {
            None
        }
    }
}

impl Iterator for MultipartReader {
    type Item = Bytes;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_part()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_http_response_parts() {
        let not_modified = "HTTP/1.1 304 Not Modified\n\
              ETag: \"etag/animals\"\n\
              \n\
              ";
        let raw_response = Bytes::from_static(not_modified.as_bytes());

        let (builder, remaining) = parse_http_response(raw_response.clone()).expect("can parse");
        let response = builder.body(()).unwrap();

        let not_modified = hyper::StatusCode::from_u16(304).unwrap();
        let etag_value = hyper::header::HeaderValue::from_static("\"etag/animals\"");

        assert_eq!(remaining, raw_response.len());
        assert_eq!(response.status(), not_modified);
        assert_eq!(response.headers().get("ETag"), Some(&etag_value));
    }

    #[test]
    fn test_read_http_response() {
        let headers = "Content-Type: application/http\n\
            Content-ID: response-{id}\n\
            \n\
            HTTP/1.1 200 OK\n\
            Content-Type: application/json\n\
            Content-Length: 156\n\
            \n\
              ";
        let body = "{\n\
            \"kind\": \"farm#animal\",\n\
            \"etag\": \"etag/pony\",\n\
            \"selfLink\": \"/farm/v1/animals/pony\",\n\
            \"animalName\": \"pony\",\n\
            \"animalAge\": 34,\n\
            \"peltColor\": \"white\"\n\
          }\n\
          ";
        let raw_body = Bytes::from(body.as_bytes());
        let raw_response = [headers, body].concat();
        let raw_response = Bytes::copy_from_slice(raw_response.as_bytes());

        let response = parse_multipart_response(raw_response).expect("can parse response");

        let ok = hyper::StatusCode::from_u16(200).unwrap();
        let ct_value = hyper::header::HeaderValue::from_static("application/json");
        let cl_value = hyper::header::HeaderValue::from_static("156");

        assert_eq!(response.status(), ok);
        assert_eq!(response.headers().get("Content-Type"), Some(&ct_value));
        assert_eq!(response.headers().get("Content-Length"), Some(&cl_value));

        assert_eq!(response.body(), &raw_body);
    }

    #[test]
    fn test_read_http_response_invalid_content_type() {
        let raw_response = "Content-Type: application/json\n\
            Content-ID: response-{id}\n\
            \n\
            HTTP/1.1 200 OK\n\
            Content-Type: application/json\n\
            Content-Length: 156\n\
            \n\
              ";
        let raw_response = Bytes::copy_from_slice(raw_response.as_bytes());

        match parse_multipart_response(raw_response) {
            Err(ReadMultipartError::InvalidPartContentType) => (),
            res => assert_eq!(
                true, false,
                "Expecting InvalidPartContentType but {:?} found",
                res
            ),
        }
    }

    #[test]
    fn test_read_parts() {
        let boundary = "boundary";
        let raw = "\r\n--boundary\r\n\
                Content-ID: response-{id1}\n\
                \n\
                HTTP/1.1 200 OK\n\
                \n\
                \r\n--boundary\r\n\
                Content-ID: response-{id22}\n\
                \n\
                HTTP/1.1 200 OK\n\
                \n\
                \r\n--boundary\r\n\
                Content-ID: response-{id333}\n\
                \n\
                HTTP/1.1 200 OK\n\
                \n\
                \r\n--boundary--\r\n\
              ";

        let bytes = Bytes::from(raw.as_bytes());
        let mut reader = MultipartReader::new(bytes, boundary).map(|b| b.len());

        assert_eq!(reader.next(), Some(45));
        assert_eq!(reader.next(), Some(46));
        assert_eq!(reader.next(), Some(47));
        assert_eq!(reader.next(), None);
        assert_eq!(reader.next(), None);
    }
}
