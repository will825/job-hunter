//! Job-board fetchers.
//!
//! Each ATS (Greenhouse now; Lever, Ashby next) implements the [`Fetcher`]
//! trait: given a board token, return a `Vec<Job>` in the app's normalized
//! shape. The orchestrator in `main.rs` doesn't know or care which board a
//! fetcher wraps — that's the whole point of the trait, and it's what makes
//! "add Lever next" a matter of writing one new file.

use anyhow::Result;

use crate::models::Job;
use crate::sources::Source;

pub mod adzuna;
pub mod ashby;
pub mod greenhouse;
pub mod himalayas;
pub mod jobicy;
pub mod lever;
pub mod remoteok;
pub mod remotive;

use adzuna::AdzunaFetcher;
use ashby::AshbyFetcher;
use greenhouse::GreenhouseFetcher;
use himalayas::HimalayasFetcher;
use jobicy::JobicyFetcher;
use lever::LeverFetcher;
use remoteok::RemoteOkFetcher;
use remotive::RemotiveFetcher;

/// A source of job postings for a single company board.
#[allow(async_fn_in_trait)] // fine for an internal trait we don't need `Send` bounds on yet
pub trait Fetcher {
    /// Short identifier stored on each job as its `source` (e.g. "greenhouse").
    fn source_name(&self) -> &'static str;

    /// Fetch all currently-listed jobs for this board.
    async fn fetch(&self, client: &reqwest::Client) -> Result<Vec<Job>>;
}

/// Dispatch to the right concrete fetcher for a source. The one place that maps
/// a [`Source`] variant to its fetcher — used by both the scan and the URL
/// detector's validation step.
pub async fn fetch_source(source: &Source, client: &reqwest::Client) -> Result<Vec<Job>> {
    match source {
        Source::Greenhouse(t) => GreenhouseFetcher::new(t.clone()).fetch(client).await,
        Source::Lever(t) => LeverFetcher::new(t.clone()).fetch(client).await,
        Source::Ashby(t) => AshbyFetcher::new(t.clone()).fetch(client).await,
        Source::Remotive(t) => RemotiveFetcher::new(t.clone()).fetch(client).await,
        Source::RemoteOk(t) => RemoteOkFetcher::new(t.clone()).fetch(client).await,
        Source::Adzuna(q) => AdzunaFetcher::new(q.clone()).fetch(client).await,
        Source::Himalayas(t) => HimalayasFetcher::new(t.clone()).fetch(client).await,
        Source::Jobicy(t) => JobicyFetcher::new(t.clone()).fetch(client).await,
        // Custom pages are handled by the custom-page reader, not here.
        Source::CustomPage(_) => Err(anyhow::anyhow!("custom pages are read by the custom-page reader")),
    }
}
