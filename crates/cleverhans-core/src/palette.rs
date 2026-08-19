//! Command-palette matching over registry display metadata (spec §4.3,
//! non-normative).
//!
//! The registry already tells the *model* what it may propose; `display`
//! metadata tells the *user* the same thing. This module is the shared
//! matcher so every frontend palette behaves identically — ship the
//! registry document, get discoverability for free.
//!
//! Only actions carrying [`DisplayDef`] participate: `description` is
//! written for the model and makes a poor (often misleading) search
//! surface, so absence of `display` means absence from the palette.

use crate::registry::ActionDef;
use crate::schema::RegistrySchema;

/// Prefix-token match: every whitespace-separated query token must be a
/// prefix of some word in the action's display title or keywords,
/// case-insensitive. Empty query matches nothing (a palette shows nothing,
/// not everything, until the user types).
pub fn match_actions<'a>(actions: &'a [ActionDef], query: &str) -> Vec<&'a ActionDef> {
    let tokens: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .map(String::from)
        .collect();
    if tokens.is_empty() {
        return Vec::new();
    }
    actions
        .iter()
        .filter(|action| {
            let Some(display) = &action.display else {
                return false;
            };
            let words: Vec<String> = display
                .title
                .to_lowercase()
                .split_whitespace()
                .map(String::from)
                .chain(display.keywords.iter().map(|k| k.to_lowercase()))
                .collect();
            tokens
                .iter()
                .all(|token| words.iter().any(|word| word.starts_with(token)))
        })
        .collect()
}

impl RegistrySchema {
    /// Palette match over this document's actions — see [`match_actions`].
    pub fn match_actions(&self, query: &str) -> Vec<&ActionDef> {
        match_actions(&self.actions, query)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::DisplayDef;

    fn action(id: &str, display: Option<DisplayDef>) -> ActionDef {
        ActionDef {
            id: id.to_owned(),
            description: "model-facing text that must not leak into matching".to_owned(),
            params: Vec::new(),
            block_type: "confirm".to_owned(),
            mutates: false,
            authz_key: id.to_owned(),
            display,
        }
    }

    fn display(title: &str, keywords: &[&str]) -> DisplayDef {
        DisplayDef {
            title: title.to_owned(),
            keywords: keywords.iter().map(|k| (*k).to_owned()).collect(),
            ..DisplayDef::default()
        }
    }

    #[test]
    fn every_token_must_prefix_a_word() {
        let actions = vec![
            action(
                "brief",
                Some(display(
                    "Generate onboarding brief",
                    &["summary", "context"],
                )),
            ),
            action(
                "plan",
                Some(display("Draft 30-60-90 plan", &["goals", "ramp"])),
            ),
        ];

        let hits = match_actions(&actions, "onboarding");
        assert_eq!(
            hits.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            ["brief"]
        );

        // Both tokens must land, on any word or keyword.
        assert_eq!(match_actions(&actions, "gen sum").len(), 1);
        assert!(match_actions(&actions, "onboarding zzz").is_empty());

        // Case-insensitive, keyword prefixes count.
        assert_eq!(match_actions(&actions, "RAMP").len(), 1);
    }

    #[test]
    fn no_display_means_no_palette_presence() {
        let actions = vec![action("hidden", None)];
        assert!(match_actions(&actions, "hidden").is_empty());
    }

    #[test]
    fn empty_query_matches_nothing() {
        let actions = vec![action(
            "brief",
            Some(display("Generate onboarding brief", &[])),
        )];
        assert!(match_actions(&actions, "").is_empty());
        assert!(match_actions(&actions, "   ").is_empty());
    }
}
