# rmsync

A collection of tools to synchronise content with the remarkable cloud.

Primarily designed for use with the fanfiction.net website. Unfortunately Cloudflare ended up detecting the scraping as bots and block it. Keeping this repo unarchived in case I want to add more content sources in the future.

For FF.net specifically, see the android application [reSync](https://github.com/fmonniot/resync/). It uses a web browser to bypass the cloudflare bot detection.

## Software

This tool is primarily written in [Rust](https://www.rust-lang.org/) and offer some nice (if somewhat opinionated) libraries:

- `crates/fanfictionnet` offer an interface to get stories out of the website (can trigger Cloudflare bot detection)
- `crates/google-cloud`, a simple API to access some gmail and cloud datastore features
- `crates/rmcloud`, an API to upload and list documents from the [remarkable cloud](https://my.remarkable.com/)

