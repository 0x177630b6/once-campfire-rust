//! Link unfurling: `UnfurlLinksController#create` (reference/app/controllers/unfurl_links_controller.rb)
//! over `Opengraph::Metadata`, `Location`, `Fetch` and `Document` (reference/app/models/opengraph).
//!
//! Every address is resolved through the private network guard and pinned, every redirect is
//! re-checked, and documents are capped at 5MB and 10 responses.

mod document;
mod entities;
mod fetch;
mod html;
mod location;
mod metadata;

pub use metadata::Metadata;

use crate::integrations::net::Network;

/// What `UnfurlLinksController#create` responds with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unfurl {
    /// `render json: opengraph` (200, `application/json`): the body.
    Json(String),
    /// `head :no_content`
    NoContent,
}

/// Where the Rails action raises (a 500).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UnfurlError {
    #[error("raised {0}")]
    Raised(&'static str),
}

/// The action after `params.require(:url)` (a missing or blank `url` is the controller's 400).
pub async fn unfurl(net: &Network, url: &str) -> Result<Unfurl, UnfurlError> {
    let mut opengraph = Metadata::from_url(net, url).await?;
    Ok(if opengraph.validate(net).await? { Unfurl::Json(opengraph.to_json()) } else { Unfurl::NoContent })
}

#[cfg(test)]
mod tests;
