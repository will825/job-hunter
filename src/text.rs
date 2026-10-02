//! Text utilities: HTML→plain-text, field normalization, and the fuzzy
//! `dedup_key` used to collapse the same job seen from multiple sources.
//!
//! All dependency-free and cheap — this runs over every job on every fetch,
//! so it stays simple string work with no regex engine or HTML parser pulled in.

use sha2::{Digest, Sha256};

/// Convert an HTML fragment to readable plain text: block tags become
/// newlines, all other tags are dropped, HTML entities are decoded, and runs
/// of whitespace are collapsed.
///
/// This is intentionally a lightweight stripper, not a real HTML parser. Job
/// descriptions are simple enough that a parser would be wasted weight.
///
/// It runs **decode → strip → decode** to handle Greenhouse's *double-escaped*
/// content (where the JSON holds `&lt;p&gt;` and `&amp;nbsp;`): the first
/// decode turns those back into real tags/entities, the strip removes the tags,
/// and the second decode resolves the now-unwrapped entities like `&nbsp;`.
/// For already-raw HTML (Ashby's `descriptionHtml`) the passes are harmless.
pub fn html_to_text(input: &str) -> String {
    let decoded = decode_entities(input);
    let stripped = strip_tags(&decoded);
    let decoded_again = decode_entities(&stripped);
    collapse_whitespace(&decoded_again)
}

/// The visible text of a whole HTML page: like [`html_to_text`], but first
/// drops `<script>`, `<style>`, `<noscript>` and `<template>` blocks, whose
/// contents (bundled UI strings, JSON) aren't what the page says.
pub fn page_text(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    'outer: while i < html.len() {
        for tag in ["script", "style", "noscript", "template"] {
            let open = format!("<{tag}");
            if lower[i..].starts_with(&open) {
                let close = format!("</{tag}");
                match lower[i..].find(&close) {
                    Some(end) => {
                        let after = i + end;
                        i = lower[after..].find('>').map_or(html.len(), |gt| after + gt + 1);
                    }
                    None => i = html.len(),
                }
                continue 'outer;
            }
        }
        let ch = html[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    html_to_text(&out)
}

/// Remove HTML tags, turning block-level tags into newlines so paragraph and
/// list structure survives as line breaks. Does not touch entities.
fn strip_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '<' {
            // Consume through the closing '>'.
            let mut raw = String::new();
            for tc in chars.by_ref() {
                if tc == '>' {
                    break;
                }
                raw.push(tc);
            }
            // Tag name = leading alphabetic run (after an optional '/').
            let trimmed = raw.trim_start_matches('/').trim_start();
            let tag: String = trimmed
                .chars()
                .take_while(|c| c.is_ascii_alphabetic())
                .map(|c| c.to_ascii_lowercase())
                .collect();
            if matches!(
                tag.as_str(),
                "p" | "br" | "li" | "div" | "tr" | "h1" | "h2" | "h3" | "h4" | "ul" | "ol"
            ) {
                out.push('\n');
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Decode HTML entities (named and numeric) in a string, leaving everything
/// else untouched.
fn decode_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '&' {
            out.push(c);
            continue;
        }
        // Read the entity body up to ';' (bounded, to avoid runaway scans).
        let mut ent = String::new();
        let mut matched = false;
        for _ in 0..12 {
            match chars.peek() {
                Some(&';') => {
                    chars.next();
                    matched = true;
                    break;
                }
                Some(&ch) if ch != '&' && !ch.is_whitespace() => {
                    ent.push(ch);
                    chars.next();
                }
                _ => break,
            }
        }
        if matched {
            out.push_str(&decode_entity(&ent));
        } else {
            out.push('&');
            out.push_str(&ent);
        }
    }
    out
}

fn decode_entity(ent: &str) -> String {
    match ent {
        "amp" => "&".into(),
        "lt" => "<".into(),
        "gt" => ">".into(),
        "quot" => "\"".into(),
        "apos" | "#39" => "'".into(),
        "nbsp" => " ".into(),
        "mdash" | "#8212" => "—".into(),
        "ndash" | "#8211" => "–".into(),
        "rsquo" | "#8217" => "'".into(),
        "lsquo" | "#8216" => "'".into(),
        "ldquo" | "#8220" => "\"".into(),
        "rdquo" | "#8221" => "\"".into(),
        "hellip" | "#8230" => "…".into(),
        other => {
            // Numeric entities: &#NN; and &#xHH;
            if let Some(hex) = other.strip_prefix("#x").or_else(|| other.strip_prefix("#X")) {
                if let Ok(n) = u32::from_str_radix(hex, 16) {
                    if let Some(ch) = char::from_u32(n) {
                        return ch.to_string();
                    }
                }
            } else if let Some(dec) = other.strip_prefix('#') {
                if let Ok(n) = dec.parse::<u32>() {
                    if let Some(ch) = char::from_u32(n) {
                        return ch.to_string();
                    }
                }
            }
            // Unknown entity: leave it visible rather than dropping content.
            format!("&{other};")
        }
    }
}

/// Collapse any run of whitespace to a single space, but preserve paragraph
/// breaks (a blank line between blocks) so descriptions stay readable.
fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut newlines = 0u8;
    let mut pending_space = false;
    for c in s.chars() {
        if c == '\n' {
            newlines = newlines.saturating_add(1);
            pending_space = false;
        } else if c.is_whitespace() {
            if newlines == 0 {
                pending_space = true;
            }
        } else {
            if newlines >= 2 {
                out.push_str("\n\n");
            } else if newlines == 1 {
                out.push('\n');
            } else if pending_space && !out.is_empty() {
                out.push(' ');
            }
            newlines = 0;
            pending_space = false;
            out.push(c);
        }
    }
    out.trim().to_string()
}

// --- Normalization for dedup ---

/// Normalize a company name for matching: lowercase, strip common corporate
/// suffixes and punctuation, collapse spaces.
pub fn normalize_company(company: &str) -> String {
    let lower = company.to_lowercase();
    let cleaned = strip_punct(&lower);
    let words: Vec<&str> = cleaned
        .split_whitespace()
        .filter(|w| !matches!(*w, "inc" | "llc" | "ltd" | "co" | "corp" | "corporation" | "the"))
        .collect();
    words.join(" ")
}

/// Normalize a job title for matching.
///
/// Deliberately **conservative**: it only folds away truly-cosmetic differences
/// — case, punctuation, and unambiguous seniority abbreviations (`Sr.`→senior).
/// It intentionally KEEPS parenthetical qualifiers and level markers (II, I,
/// "German Speaking", "Nordics/UK") as part of the key, because those routinely
/// distinguish *genuinely different* postings. Merging them would silently drop
/// a real job — a far worse failure than showing two near-identical rows.
pub fn normalize_title(title: &str) -> String {
    // Punctuation → spaces keeps the words inside "(German Speaking)" while
    // discarding the brackets/slashes, so distinct qualifiers stay distinct.
    let cleaned = strip_punct(&title.to_lowercase());
    let words: Vec<&str> = cleaned
        .split_whitespace()
        .map(|w| match w {
            "sr" => "senior",
            "jr" => "junior",
            "mgr" => "manager",
            other => other,
        })
        .collect();
    words.join(" ")
}

/// Normalize a location: lowercase, map any remote variant to "remote",
/// strip punctuation.
pub fn normalize_location(location: &str) -> String {
    let lower = location.to_lowercase();
    if lower.contains("remote") {
        return "remote".to_string();
    }
    strip_punct(&lower).split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_punct(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() || c.is_whitespace() { c } else { ' ' })
        .collect()
}

/// The fuzzy dedup key: a hash of normalized company + title + location.
/// Two postings that reduce to the same key are treated as the same job,
/// even across sources and across "Engineer" vs "Engineer II".
pub fn dedup_key(company: &str, title: &str, location: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(normalize_company(company).as_bytes());
    hasher.update(b"\x1f");
    hasher.update(normalize_title(title).as_bytes());
    hasher.update(b"\x1f");
    hasher.update(normalize_location(location).as_bytes());
    format!("{:x}", hasher.finalize())
}

/// A display name for a board token: "deepgram" → "Deepgram",
/// "hook-music" → "Hook Music". Used when a board gives no company name.
pub fn company_from_token(token: &str) -> String {
    token
        .split(['-', '_', '.'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Hash of normalized company + title, *without* location: the key that
/// groups one aggregator posting that was re-listed once per city.
pub fn title_key(company: &str, title: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(normalize_company(company).as_bytes());
    hasher.update(b"\x1f");
    hasher.update(normalize_title(title).as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_text_drops_scripts_and_styles() {
        let html = "<html><head><style>p{}</style><SCRIPT>t('This job is no longer available')</SCRIPT></head>\
                    <body><p>Senior Engineer</p><noscript>enable JS</noscript>é</body></html>";
        let text = page_text(html);
        assert!(text.contains("Senior Engineer") && text.contains('é'));
        assert!(!text.contains("no longer available") && !text.contains("enable JS") && !text.contains("p{}"));
    }

    #[test]
    fn company_from_token_title_cases_words() {
        assert_eq!(company_from_token("deepgram"), "Deepgram");
        assert_eq!(company_from_token("hook-music"), "Hook Music");
        assert_eq!(company_from_token("some_co.io"), "Some Co Io");
        assert_eq!(company_from_token(""), "");
    }

    #[test]
    fn strips_html_and_decodes_entities() {
        let html = "<p>Build <strong>Rust</strong> &amp; audio tools</p><li>DSP</li>";
        let text = html_to_text(html);
        assert!(text.contains("Build Rust & audio tools"));
        assert!(text.contains("DSP"));
        assert!(!text.contains('<'));
    }

    #[test]
    fn double_escaped_greenhouse_content_is_cleaned() {
        // This is how Greenhouse actually delivers content: tags and entities
        // are themselves entity-escaped in the JSON.
        let raw = "&lt;div class=\"x\"&gt;&lt;p&gt;&lt;strong&gt;WHO WE ARE:&amp;nbsp;&lt;/strong&gt;&lt;/p&gt;&lt;p&gt;Splice &amp;amp; audio&lt;/p&gt;&lt;/div&gt;";
        let text = html_to_text(raw);
        assert!(!text.contains('<'), "no tags should survive: {text:?}");
        assert!(!text.contains("&nbsp;") && !text.contains("&amp;"), "entities resolved: {text:?}");
        assert!(text.contains("WHO WE ARE:"));
        assert!(text.contains("Splice & audio"));
    }

    #[test]
    fn cosmetic_abbreviations_normalize() {
        // Truly-cosmetic differences fold together.
        assert_eq!(normalize_title("Sr. Software Engineer"), "senior software engineer");
    }

    #[test]
    fn cosmetic_only_differences_share_a_key() {
        // Same job differing only by company suffix + remote-location wording.
        let a = dedup_key("Splice", "Senior Product Manager", "Remote - U.S.");
        let b = dedup_key("Splice Inc.", "Sr Product Manager", "Remote");
        assert_eq!(a, b, "cosmetic-only differences should share a key");
    }

    #[test]
    fn distinct_parenthetical_qualifiers_do_not_merge() {
        // The real-world false-merge we must avoid: two different regional roles.
        let a = dedup_key("Spotify", "Client Partner (German Speaking)", "London");
        let b = dedup_key("Spotify", "Client Partner (Nordics / UK)", "London");
        assert_ne!(a, b, "different qualifiers are different jobs — must not collapse");
    }

    #[test]
    fn distinct_levels_do_not_merge() {
        // A company may post both levels as separate openings; keep them both.
        let a = dedup_key("Spotify", "Android Engineer I", "London");
        let b = dedup_key("Spotify", "Android Engineer II", "London");
        assert_ne!(a, b);
    }

    #[test]
    fn different_roles_differ() {
        let a = dedup_key("Splice", "Product Manager", "Remote");
        let b = dedup_key("Splice", "Software Engineer", "Remote");
        assert_ne!(a, b);
    }
}
