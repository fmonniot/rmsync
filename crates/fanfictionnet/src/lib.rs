use hyper::{body, Client};
use scraper::{Html, Selector, Node};
use hyper_tls::HttpsConnector;
use log::warn;
use ego_tree::NodeRef;

#[derive(Debug, PartialEq)]
pub struct StoryId(u32);

pub fn new_story_id(id: u32) -> StoryId {
    StoryId(id)
}

#[derive(PartialEq, Debug, Copy, Clone)]
pub struct ChapterNum(u16);

pub fn new_chapter_number(num: u16) -> ChapterNum {
    ChapterNum(num)
}

const FFN_BASE_URL: &str = "https://www.fanfiction.net";

#[derive(Debug)]
pub enum Error {
    Http(hyper::Error),
}

impl From<hyper::Error> for Error {
    fn from(error: hyper::Error) -> Self {
        Error::Http(error)
    }
}

pub struct Chapter {
    num: ChapterNum,
    title: String,
    story_name: String,
    author: String,
    content: String,
}

impl Chapter {
    pub fn number(&self) -> ChapterNum {
        self.num
    }

    pub fn title(&self) -> &String {
        &self.title
    }

    pub fn story_title(&self) -> &String {
        &self.story_name
    }

    pub fn author(&self) -> &String {
        &self.author
    }

    pub fn content(&self) -> &String {
        &self.content
    }
}

pub async fn fetch_story_chapter(sid: StoryId, chapter: ChapterNum) -> Result<Chapter, Error> {
    let https = HttpsConnector::new();
    let client = Client::builder().build::<_, hyper::Body>(https);

    let url = format!("{}/s/{}/{}", FFN_BASE_URL, sid.0, chapter.0)
        .parse()
        .unwrap();
    let mut resp = client.get(url).await?;

    // TODO Add a status check

    let bytes = body::to_bytes(resp.body_mut()).await?;
    let content = String::from_utf8(bytes.to_vec()).expect("response was not valid utf-8");
    let chapter = parse_chapter(content, chapter);

    Ok(chapter)
}

// TODO Return error instead of panicking
fn parse_chapter(raw_html: String, chapter: ChapterNum) -> Chapter {
    let document = Html::parse_document(&raw_html);

    let selector = Selector::parse(".storytext").unwrap();
    let mut buffer = Vec::new();
    serialize_tree(&mut buffer, &document.select(&selector).next().unwrap());
    let content = String::from_utf8(buffer).unwrap();

    let selector = Selector::parse("#profile_top > b.xcontrast_txt").unwrap();
    let story_name = document.select(&selector).next().unwrap().inner_html();

    let selector = Selector::parse("#chap_select > option[selected]").unwrap();
    let title = document.select(&selector).next().unwrap().inner_html();

    let selector = Selector::parse("#profile_top > a:nth-child(5)").unwrap();
    let author = document.select(&selector).next().unwrap().inner_html();

    Chapter {
        num: chapter,
        title,
        story_name,
        author,
        content,
    }
}

const TAG_LESSER: u8 = '<' as u8;
const TAG_GREATER: u8 = '>' as u8;
const TAG_CLOSING: &[u8] = "</".as_bytes();
const TAG_EQUAL: u8 = '=' as u8;
const TAG_QUOTE: u8 = '"' as u8;
const TAG_SPACE: u8 = ' ' as u8;

/// Because we are in need to convert from HTML to xHTML, we can't
/// use the builtin `.inner_html` function. So instead we build a
/// very simple tree traverser to serialize the content.
///
/// Because the story text on FF.net isn't very deep (usually just two
/// levels), we use a simple recursion (no real stack overflow risk).
fn serialize_tree(buffer: &mut Vec<u8>, r: &NodeRef<Node>) {
    for node in r.children() {
        match node.value() {
            Node::Element(el) => {
                // start tag
                buffer.push(TAG_LESSER);
                // tag name
                buffer.extend_from_slice(el.name().as_bytes());
                // attributes
                for (name, value) in el.attrs() {
                    buffer.push(TAG_SPACE);
                    buffer.extend_from_slice(name.as_bytes());
                    buffer.push(TAG_EQUAL);
                    buffer.push(TAG_QUOTE);
                    buffer.extend_from_slice(value.as_bytes());
                    buffer.push(TAG_QUOTE);
                }
                // end tag
                buffer.push(TAG_GREATER);

                // content (recursion)
                serialize_tree(buffer, &node);

                // start end tag
                buffer.extend_from_slice(TAG_CLOSING);
                // tag name
                buffer.extend_from_slice(el.name().as_bytes());
                // end end tag
                buffer.push(TAG_GREATER);
            }
            Node::Text(txt) => {
                let s: &[u8] = (*txt.text).as_bytes();

                buffer.extend_from_slice(s);
            }
            _ => warn!("unsupported node type encountered"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn asset(p: &str) -> String {
        let mut d = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        d.push("assets");
        d.push(p);

        std::fs::read_to_string(d).unwrap()
    }

    #[test]
    fn parse_one_chapter() {
        let ch = parse_chapter(asset("4985743_38.html"), ChapterNum(1));

        assert_eq!(ch.story_name, "The Path of a Jedi");
        assert_eq!(ch.title, "38. Part III, Chapter 1");
        assert_eq!(ch.num, ChapterNum(1));
        assert_eq!(ch.content.len(), 31430);
    }
}
