use std::time::Duration;

use anyhow::{anyhow, Result};
use chrono::{offset, DateTime};
use iso8601_duration::Duration as IsoDuration;
use serde::Deserialize;
use url::Url;

use crate::{config::YouTubeConfig, irc_string::IrcString};

#[derive(Clone, Debug, PartialEq)]
pub struct YouTube {
    pub id: IrcString,
    pub title: IrcString,       // items[0]/snippet/title
    pub description: IrcString, // items[0]/snippet/description
    pub published_at: Option<DateTime<offset::FixedOffset>>, // items[0]/snippet/published_at
    pub duration: Duration,     // items[0]/contentDetails/duration
    pub channel: IrcString,     // items[0]/snippet/channelTitle
    pub views: u64,             // items[0]/statistics/viewCount
    pub likes: u64,             // items[0]/statistics/likeCount
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct YouTubeResponse {
    items: Vec<YouTubeItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct YouTubeItem {
    id: String,
    snippet: YouTubeSnippet,
    content_details: YouTubeDetails,
    statistics: YouTubeStats,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct YouTubeSnippet {
    title: String,
    description: String,
    channel_title: String,
    published_at: String,
    localized: YouTubeLocalised,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct YouTubeLocalised {
    title: String,
    description: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct YouTubeDetails {
    duration: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct YouTubeStats {
    view_count: String,
    like_count: String,
}

impl From<YouTubeItem> for YouTube {
    fn from(y: YouTubeItem) -> Self {
        YouTube {
            id: y.id.into(),
            title: y.snippet.localized.title.into(),
            description: y.snippet.localized.description.into(),
            channel: y.snippet.channel_title.into(),
            published_at: DateTime::parse_from_rfc3339(&y.snippet.published_at).ok(),
            duration: IsoDuration::parse(&y.content_details.duration)
                .map(|d| d.to_std())
                .unwrap_or_default(),
            views: y.statistics.view_count.parse().unwrap_or_default(),
            likes: y.statistics.like_count.parse().unwrap_or_default(),
        }
    }
}

fn maybe_id(id: &str) -> Option<String> {
    if id.len() == 11
        && id
            .chars()
            .all(|c| matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-'))
    {
        Some(id.to_string())
    } else {
        None
    }
}

pub fn extract_youtube_id(url: &Url) -> Option<String> {
    match url.domain()? {
        "youtu.be" => maybe_id(url.path_segments()?.next()?),
        "www.youtube.com" | "youtube.com" => match url.path_segments()?.next()? {
            "shorts" | "embed" => maybe_id(url.path_segments()?.nth(1)?),
            "watch" => url
                .query_pairs()
                .find(|(key, _)| key == "v")
                .and_then(|(_, val)| maybe_id(&val)),
            _ => None,
        },
        _ => None,
    }
}

#[test]
fn test_extract_youtube_id() {
    assert_eq!(
        extract_youtube_id(&Url::parse("https://youtu.be/a123456789Z").unwrap()),
        Some("a123456789Z".to_string())
    );
    assert_eq!(
        extract_youtube_id(&Url::parse("https://youtu.be/a123@56789Z").unwrap()),
        None
    );
    assert_eq!(
        extract_youtube_id(&Url::parse("https://youtu.be/a123456789zZ").unwrap()),
        None
    );
    assert_eq!(
        extract_youtube_id(&Url::parse("https://youtube.com/watch?v=a123456789Z&t=42m").unwrap()),
        Some("a123456789Z".to_string())
    );
    assert_eq!(
        extract_youtube_id(&Url::parse("https://www.youtube.com/watch?v=a123456789Z").unwrap()),
        Some("a123456789Z".to_string())
    );
    assert_eq!(
        extract_youtube_id(&Url::parse("https://youtube.com/shorts/a123456789Z").unwrap()),
        Some("a123456789Z".to_string())
    );
    assert_eq!(
        extract_youtube_id(&Url::parse("https://youtube.com/embed/a123456789Z").unwrap()),
        Some("a123456789Z".to_string())
    );
    assert_eq!(
        extract_youtube_id(&Url::parse("https://www.youtube.com/a123456789Z").unwrap()),
        None
    );
    assert_eq!(
        extract_youtube_id(&Url::parse("https://youtube/a123456789Z").unwrap()),
        None
    );
}

pub async fn youtube_lookup(id: &str, config: &YouTubeConfig) -> Result<YouTube> {
    let client = reqwest::Client::new();
    let mut response = client
        .get("https://www.googleapis.com/youtube/v3/videos")
        .query(&[
            ("id", id),
            ("key", &config.api_key.clone().unwrap_or_default()),
            ("hl", &config.lang.clone().unwrap_or_default()),
            ("part", "snippet,contentDetails,statistics"),
        ])
        .send()
        .await?
        .json::<YouTubeResponse>()
        .await?;

    if let Some(item) = response.items.pop() {
        Ok(item.into())
    } else {
        Err(anyhow!("No items in response"))
    }
}
