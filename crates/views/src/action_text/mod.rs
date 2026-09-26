//! Views for `reference/app/views/action_text` (attachable partials rendered inside rich text).

use askama::Template;

#[allow(unused_imports)]
use crate::helpers::{self as h, filters};

/// `ActionText::Attachment::OpengraphEmbed` (lib/rails_ext/actiontext_opengraph_embeds.rb),
/// with `href`/`url` already vetted by `web_url`.
#[derive(Clone, Debug, Default)]
pub struct OpengraphEmbed {
    pub href: Option<String>,
    pub url: Option<String>,
    pub filename: String,
    pub description: Option<String>,
}

impl OpengraphEmbed {
    pub fn twitter_avatar(&self) -> bool {
        self.url.as_deref().unwrap_or("").starts_with("https://pbs.twimg.com/profile_images")
    }
}

/// `action_text/attachables/_opengraph_embed.html.erb`.
#[derive(Template)]
#[template(path = "action_text/attachables/_opengraph_embed.html")]
pub struct OpengraphEmbedPartial {
    pub opengraph_embed: OpengraphEmbed,
}
