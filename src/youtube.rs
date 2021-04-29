use std::time::Duration;

use anyhow::{anyhow, Result};
use iso8601_duration::Duration as IsoDuration;
use serde::Deserialize;
use url::Url;

use crate::irc_string::IrcString;

#[derive(Clone, Debug, PartialEq)]
pub struct YouTube {
    pub id: IrcString,
    pub title: IrcString,       // items[0]/snippet/title
    pub description: IrcString, // items[0]/snippet/description
    pub duration: Duration,     // items[0]/contentDetails/duration
    pub channel: IrcString,     // items[0]/snippet/channelTitle
    pub views: u64,             // items[0]/statistics/viewCount
    pub likes: u64,             // items[0]/statistics/likeCount
    pub dislikes: u64,          // items[0]/statistics/dislikeCount
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
    dislike_count: String,
}

impl From<YouTubeItem> for YouTube {
    fn from(y: YouTubeItem) -> Self {
        YouTube {
            id: y.id.into(),
            title: y.snippet.title.into(),
            description: y.snippet.description.into(),
            channel: y.snippet.channel_title.into(),
            duration: IsoDuration::parse(&y.content_details.duration)
                .map(|d| d.to_std())
                .unwrap_or_default(),
            views: y.statistics.view_count.parse().unwrap_or_default(),
            likes: y.statistics.like_count.parse().unwrap_or_default(),
            dislikes: y.statistics.dislike_count.parse().unwrap_or_default(),
        }
    }
}

pub fn extract_youtube_id(url: &Url) -> Option<String> {
    rustube::Id::from_raw(url.as_str())
        .ok()
        .map(|s| s.as_str().to_string())
}

pub async fn youtube_lookup(id: &str, key: &str) -> Result<YouTube> {
    let client = reqwest::Client::new();
    let mut response = client
        .get("https://www.googleapis.com/youtube/v3/videos")
        .query(&[
            ("id", id),
            ("key", key),
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
