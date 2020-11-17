# annoirc - annotate IRC

A simple, efficient IRC bot for annotating Internet Relay Chat conversations.

## Synopsis

```
<@Freaky> https://www.rust-lang.org/
<annobot> [www.rust-lang.org] Rust Programming Language
<annobot> [www.rust-lang.org] A language empowering everyone to build reliable and efficient software.
<@Freaky> https://twitter.com/DwarfFortBugs/status/1321309817718427654
<annobot> [Twitter] Dwarf Fortress Bugs (@DwarfFortBugs) 9244: Scribes rarely, if ever, actually work in the library | ❤️69 2020-10-28 04:36
```

## Description

`annobot` is an IRC bot meant to respond to unauthenticated users with information
to help enhance the Chat Experience™.  This includes:

* Title text and meta descriptions of websites.
* Detailed information for Tweets and Twitter users via API.
* Pretty colours.
* Other stuff at some point.

It's currently very young and should be deployed with care.

## Key Features

* Multi-network.
* Efficient async Rust with request caching and deduplication.
* Per-channel rate limits.
* Graceful config reloads.

## Todo

* Gracefuler config reloads.
* Actually hook up more of the configuration.
* Fully templated responses with colour support.
* Special handling for YouTube, Wikipedia, Github, Imgur, IMDB, etc.
* Functions such as weather, dictionary lookups, currency conversions, stocks, etc.
* Stop writing `clone()` everywhere.

## See Also

* [url-bot-rs](https://github.com/nuxeh/url-bot-rs), another simple Rust URL handling bot.