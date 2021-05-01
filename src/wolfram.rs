use anyhow::{anyhow, Result};
use serde::Deserialize;

use crate::irc_string::IrcString;

#[derive(Debug, Deserialize, PartialEq)]
struct Response {
    queryresult: QueryResult,
}

#[derive(Debug, Deserialize, PartialEq)]
struct QueryResult {
    success: bool,
    error: bool,
    pods: Vec<Pod>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Pod {
    title: String,
    id: String,
    error: bool,
    subpods: Vec<Subpod>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Subpod {
    plaintext: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WolframPod {
    pub id: String,
    pub title: IrcString,
    pub values: Vec<IrcString>,
}

pub async fn wolfram_query(query: &str, app_id: &str) -> Result<Vec<WolframPod>> {
    let client = reqwest::Client::new();
    let response = client
        .get("http://api.wolframalpha.com/v2/query")
        .query(&[
            ("appid", app_id),
            ("input", query),
            ("podtimeout", "0.5"),
            ("format", "plaintext"),
            ("units", "metric"),
            ("output", "json"),
        ])
        .send()
        .await?
        .json::<Response>()
        .await?;

    let pods: Vec<WolframPod> = response
        .queryresult
        .pods
        .into_iter()
        .map(|pod| WolframPod {
            id: pod.id,
            title: pod.title.into(),
            values: pod
                .subpods
                .into_iter()
                .flat_map(|p| p.plaintext)
                .filter(|s| !s.is_empty())
                .map(IrcString::from)
                .collect::<Vec<_>>(),
        })
        .filter(|item| !item.values.is_empty())
        .collect();

    if pods.len() > 1 {
        Ok(pods)
    } else {
        Err(anyhow!("No response pod"))
    }
}
