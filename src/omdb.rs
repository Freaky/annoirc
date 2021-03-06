use anyhow::{anyhow, Result};

use crate::irc_string::IrcString;

#[derive(Clone, Debug, PartialEq)]
pub struct Movie {
    pub title: IrcString,
    pub plot: IrcString,
    pub rated: IrcString,
    pub released: IrcString,
    pub runtime: IrcString,
    pub genre: IrcString,
    pub director: IrcString,
    pub imdb_rating: IrcString,
    pub imdb_votes: IrcString,
    pub imdb_id: IrcString,
    pub metascore: IrcString,
}

impl From<omdb::Movie> for Movie {
    fn from(movie: omdb::Movie) -> Self {
        Movie {
            title: movie.title.into(),
            plot: movie.plot.into(),
            rated: movie.rated.into(),
            released: movie.released.into(),
            runtime: movie.runtime.into(),
            genre: movie.genre.into(),
            director: movie.director.into(),
            imdb_rating: movie.imdb_rating.into(),
            imdb_votes: movie.imdb_votes.into(),
            imdb_id: movie.imdb_id.into(),
            metascore: movie.metascore.into(),
        }
    }
}

pub async fn imdb_id(id: &str, key: &str) -> Result<Movie> {
    let movie = omdb::imdb_id(id).apikey(key).get().await?;

    Ok(movie.into())
}

pub async fn search(query: &str, kind: &str, key: &str) -> Result<Movie> {
    let kind = match kind {
        "movie" => omdb::Kind::Movie,
        "series" => omdb::Kind::Series,
        _ => return Err(anyhow!("Unknown type")),
    };

    let movie = omdb::title(query).apikey(key).kind(kind).get().await?;

    Ok(movie.into())
}
