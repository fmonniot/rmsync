use epub_builder::EpubBuilder;
use epub_builder::EpubContent;
use epub_builder::ReferenceType;
use epub_builder::ZipLibrary;
use fanfictionnet::Chapter;
use std::io::Cursor;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Error while building an epub file: {0}")]
    Epub(#[from] epub_builder::Error),

    #[error("Attempting to generate a story with 0 chapters")]
    EmptyStory,
}

pub fn from_chapter(chapter: Chapter) -> Result<Vec<u8>, Error> {
    from_story(vec![chapter])
}

pub fn from_story(mut chapters: Vec<Chapter>) -> Result<Vec<u8>, Error> {
    if chapters.len() == 0 {
        return Err(Error::EmptyStory);
    }

    // When generating a multi chapter, add the chapter name before the start of the chapter
    let insert_chapter_name = chapters.len() > 1;

    // Sort our story by chapters
    chapters.sort_by_key(|c| c.number());
    let mut iter = chapters.into_iter();

    let mut builder = EpubBuilder::new(ZipLibrary::new()?)?;

    // First chapter is important: it defines the metadata for the entire story
    let first_chapter = iter.next().unwrap();

    // Story metadata
    builder.metadata("author", first_chapter.author())?;
    builder.metadata("title", first_chapter.story_title())?;
    builder.metadata("generator", "rmsync")?;

    // Insert the first chapter
    builder.add_content(chapter_to_epub_content(&first_chapter, insert_chapter_name))?;

    // And then all the others
    for chapter in iter {
        builder.add_content(chapter_to_epub_content(&chapter, insert_chapter_name))?;
    }

    // Finally generate the epub file itself
    let mut buffer = Vec::new();
    builder.generate(&mut buffer)?;

    Ok(buffer)
}

fn chapter_to_epub_content(
    chapter: &Chapter,
    insert_title_name: bool,
) -> EpubContent<Cursor<Vec<u8>>> {
    let title = if insert_title_name {
        format!(
            "<h2>{}</h2><hr style=\"width:100%;margin: 0 10% 0 10%;\"></hr>",
            chapter.title()
        )
    } else {
        String::new()
    };

    let content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
<body>
{}
{}
</body>
</html>"#,
        title,
        chapter.content()
    );
    let content = std::io::Cursor::new(content.into_bytes());
    let href = format!("chapter_{}.xhtml", chapter.number());

    EpubContent::new(href, content)
        .title(chapter.title())
        .reftype(ReferenceType::Text)
}
