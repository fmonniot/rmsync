use hyper::{body, Client};
use scraper::{Html, Selector};

#[derive(Debug, PartialEq)]
pub struct StoryId(u32);

pub fn new_story_id(id: u32) -> StoryId {
    StoryId(id)
}

#[derive(PartialEq, Debug)]
pub struct ChapterNum(u16);

pub fn new_chapter_number(num: u16) -> ChapterNum {
    ChapterNum(num)
}

const FFN_BASE_URL: &str = "https://www.fanfiction.net";

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
    content: String,
}

pub async fn fetch_story_chapter(sid: StoryId, chapter: ChapterNum) -> Result<Chapter, Error> {
    // Still inside `async fn main`...
    let client = Client::new();

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
    let content = document.select(&selector).next().unwrap().html();

    let selector = Selector::parse("#profile_top > b.xcontrast_txt").unwrap();
    let story_name = document.select(&selector).next().unwrap().inner_html();

    let selector = Selector::parse("#chap_select > option[selected]").unwrap();
    let title = document.select(&selector).next().unwrap().inner_html();

    Chapter {
        num: chapter,
        title,
        story_name,
        content,
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
        assert_eq!(ch.content.len(), 31465);
    }
}
