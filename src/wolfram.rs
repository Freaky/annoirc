
use anyhow::Result;

use crate::irc_string::IrcString;

pub async fn wolfram_query(query: &str, app_id: &str) -> Result<IrcString> {
    let client = reqwest::Client::new();
    let response = client.get("http://api.wolframalpha.com/v1/result")
        .query(&[("appid", app_id), ("i", query)])
        .send()
        .await?
        .text()
        .await?;

    Ok(response.into())
}
