//! data structure used on the wire by GMail APIs

use serde::Deserialize;

#[derive(Debug, Deserialize, PartialEq)]
pub struct MessageId(pub(crate) String);

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(super) struct HistoryListResponse {
    #[serde(default)]
    pub(super) history: Vec<History>,
    next_page_token: Option<String>,
    history_id: String,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(super) struct History {
    id: String,
    pub(super) messages: Vec<HistoryMessage>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(super) struct HistoryMessage {
    pub(super) id: MessageId,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub payload: MessagePart,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct MessagePart {
    pub headers: Vec<Header>,
    pub body: MessagePartBody,
    #[serde(default)]
    pub parts: Vec<MessagePart>,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct Header {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct MessagePartBody {
    size: u32,
    data: Option<String>, // TODO This is apparently optional in some cases
}

impl MessagePartBody {
    pub fn decoded_data(&self) -> Result<String, super::Error> {
        if self.size == 0 {
            Err(super::Error::NoBodyToDecode)?;
        }
        let data = self.data.as_ref().ok_or(super::Error::NoBodyToDecode)?;
        let bytes = base64::decode(data)?;
        let s = String::from_utf8(bytes)?;

        Ok(s)
    }
}
