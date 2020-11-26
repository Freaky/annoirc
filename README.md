# annoirc - annotate IRC

A simple, efficient IRC bot for annotating Internet Relay Chat conversations.

## Synopsis

```
<@Freaky> https://www.rust-lang.org/
<annobot> [www.rust-lang.org] Rust Programming Language
<annobot> [www.rust-lang.org] A language empowering everyone to build reliable
          and efficient software.
<@Freaky> https://twitter.com/DwarfFortBugs/status/1321309817718427654
<annobot> [Twitter] Dwarf Fortress Bugs (@DwarfFortBugs) 9244: Scribes rarely,
          if ever, actually work in the library | ❤️69 2020-10-28 04:36
<@Freaky> https://en.wikipedia.org/wiki/Rust_(programming_language)
<annobot> [en.wikipedia.org] Rust (programming language)
       ╰  [en.wikipedia.org] Rust is a multi-paradigm programming language
          designed for performance and safety, especially safe concurrency.
          Rust is syntactically similar to C++, but can guarantee memory safety
          by using a borrow checker to validate references. Rust achieves memory
          safety without garbage collection, and reference counting is optional.
<@Freaky> https://www.imdb.com/title/tt2543164/
<annobot> [IMDB] Arrival (11 Nov 2016) [7.9/10 with 582,367 votes, Metascore: 81]
          [PG-13] [Drama, Mystery, Sci-Fi, Thriller]
          https://www.imdb.com/title/tt2543164 - A linguist works with the
          military to communicate with alien lifeforms after twelve mysterious
          spacecrafts appear around the world.
<@Freaky> .movie 2001
<annobot> [IMDB] 2001: A Space Odyssey (12 May 1968) [8.3/10 with 594,224 votes,
          Metascore: 84] [G] [Adventure, Sci-Fi]
          https://www.imdb.com/title/tt0062622 - After discovering a mysterious
          artifact buried beneath the Lunar surface, mankind sets off on a quest
          to find its origins with help from intelligent supercomputer H.A.L.
          9000.
```

## Description

`annobot` is an IRC bot meant to respond to unauthenticated users with information
to help enhance the Chat Experience™.  This includes:

* Title text and meta descriptions of websites.
* Dedicated handling for IMDB, Twitter, and Wikipedia.
* Pretty colours.
* Other stuff at some point.

It's currently very young and should be deployed with care.

## Key Features

* Multi-network.
* Efficient async Rust with request caching and deduplication.
* Per-channel rate limits.
* Graceful config reloads via `SIGHUP`.

## Todo

* Gracefuler config reloads (any IRC changes currently involve a reconnection).
* Fully templated responses with colour support.
* Special handling for YouTube, Github, Imgur, etc.
* Functions such as weather, dictionary lookups, currency conversions, stocks, etc.
* Stop writing `clone()` everywhere.

## See Also

* [url-bot-rs](https://github.com/nuxeh/url-bot-rs), another simple Rust URL handling bot.