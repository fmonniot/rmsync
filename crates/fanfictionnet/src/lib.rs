use ego_tree::NodeRef;
use reqwest::StatusCode;
use log::{debug, warn};
use scraper::{ElementRef, Html, Node, Selector};
use tokio::time::{timeout, Duration};

#[derive(Debug, PartialEq, Copy, Clone)]
pub struct StoryId(u32);

impl StoryId {
    pub fn from_str(s: &str) -> Option<StoryId> {
        s.parse::<u32>().ok().map(StoryId)
    }
}

pub fn new_story_id(id: u32) -> StoryId {
    StoryId(id)
}

#[derive(PartialEq, Debug, Copy, Clone, Ord, PartialOrd, Eq)]
pub struct ChapterNum(u16);

impl ChapterNum {
    pub fn new(num: u16) -> ChapterNum {
        ChapterNum(num)
    }

    pub fn from_str(s: &str) -> Option<ChapterNum> {
        s.parse::<u16>().ok().map(ChapterNum)
    }
}

impl std::fmt::Display for ChapterNum {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

pub fn new_chapter_number(num: u16) -> ChapterNum {
    ChapterNum(num)
}

const FFN_BASE_URL: &str = "https://www.fanfiction.net";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Request to fetch chapter content failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Fetching the story from ff.net took more than a second")]
    Timeout(#[from] tokio::time::Elapsed),

    #[error("ff.net returned a non 200 response: {0}")]
    InvalidStatusCode(StatusCode),

    #[error("Couldn't format the body as a valid UTF-8 string: {0}")]
    InvalidBody(#[from] std::string::FromUtf8Error),

    #[error("No elements matched the selector {0}")]
    SelectNoResult(&'static str),
}

pub struct Chapter {
    num: ChapterNum,
    title: String,
    story_name: String,
    author: String,
    content: String,
    total_chapters: usize,
}

impl std::fmt::Debug for Chapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Chapter")
            .field("num", &self.num)
            .field("title", &self.title)
            .field("story_name", &self.story_name)
            .field("author", &self.author)
            .field("content.len", &self.content.len())
            .field("total_chapters", &self.total_chapters)
            .finish()
    }
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

    pub fn number_of_chapters(&self) -> u16 {
        self.total_chapters as u16
    }
}

pub async fn fetch_story_chapter(sid: StoryId, chapter: ChapterNum) -> Result<Chapter, Error> {
    let client = reqwest::Client::new();

    let uri = format!("{}/s/{}/{}", FFN_BASE_URL, sid.0, chapter.0);

    debug!("fetching story chapter at {}", uri);
    let resp = timeout(Duration::from_secs(2), client.get(&uri).header("user-agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/86.0.4240.198 Safari/537.36").send()).await??;

    if !resp.status().is_success() {
        return Err(Error::InvalidStatusCode(resp.status()));
    }

    let content = resp.text().await?;
    let chapter = parse_chapter(content, chapter)?;

    Ok(chapter)
}

fn parse_chapter(raw_html: String, chapter: ChapterNum) -> Result<Chapter, Error> {
    debug!("parse_chapter(chapter: {})", chapter);
    let document = Html::parse_document(&raw_html);

    // Get the content of this chapter
    let story = find_el(&document, ".storytext")?;
    let mut buffer = Vec::new();
    serialize_tree(&mut buffer, &story);
    let content = String::from_utf8(buffer).unwrap();

    // Get some story metadata
    let story_name = find_el(&document, "#profile_top > b.xcontrast_txt")?.inner_html();
    let author = find_el(&document, "#profile_top > a")?.inner_html();

    // Let's lookup the chapter selector menu
    let selector = Selector::parse("#chap_select").unwrap();
    let (title, total_chapters) = if let Some(chap_select) = document.select(&selector).next() {
        // In the chapter list, get the current chapter title
        let selector = Selector::parse("option[selected]").unwrap();
        let title = chap_select
            .select(&selector)
            .next()
            .ok_or(Error::SelectNoResult("option[selected]"))?
            .inner_html();

        // And count the total number of chapters
        let total_chapters = chap_select.children().count();

        (title, total_chapters)
    } else {
        // If there is no menu, it means it's a one shot. Let's use the story name as chapter title instead.
        (story_name.clone(), 1)
    };

    Ok(Chapter {
        num: chapter,
        title,
        story_name,
        author,
        content,
        total_chapters,
    })
}

// Because the selector is going to be a literal string, we assume it will be valid
fn find_el<'a>(doc: &'a Html, selector: &'static str) -> Result<ElementRef<'a>, Error> {
    let sel = Selector::parse(&selector).unwrap();

    doc.select(&sel)
        .next()
        .ok_or(Error::SelectNoResult(selector))
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
        let ch = parse_chapter(asset("4985743_38.html"), ChapterNum(1)).expect("parse the chapter");

        assert_eq!(ch.num, ChapterNum(1));
        assert_eq!(ch.title, "38. Part III, Chapter 1");
        assert_eq!(ch.story_name, "The Path of a Jedi");
        assert_eq!(ch.author, "mokakenobi");
        assert_eq!(ch.content.len(), 31430);
        assert_eq!(ch.total_chapters, 52);
    }

    #[test]
    fn parse_oneshot_story() {
        let ch = parse_chapter(asset("13750471_1.html"), ChapterNum(1)).expect("parse the chapter");

        assert_eq!(ch.num, ChapterNum(1));
        assert_eq!(ch.title, "Those Autumn Leaves");
        assert_eq!(ch.story_name, "Those Autumn Leaves");
        assert_eq!(ch.author, "LORDSLAYER69");
        assert_eq!(ch.content.len(), 38820);
        assert_eq!(ch.total_chapters, 1);
    }
}
