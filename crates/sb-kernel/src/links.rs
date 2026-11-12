//! Document links found in conversations (docs/specs/link-ingest.md, ADR-0022).
//!
//! Classification is by URL alone and pure: the URL is never requested here.

use sha2::{Digest, Sha256};

use crate::kinds::SourceKind;

/// A link to something that can become a `google.doc` or `microsoft.file` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRef {
    /// `google.doc` or `microsoft.file`.
    pub kind: SourceKind,
    /// `google:<fileId>`, or `ms:<sha256 of the URL without its fragment>`.
    pub key: String,
    /// The URL without its fragment. It may carry a sharing token: never log it.
    pub url: String,
    /// Lower-case host.
    pub host: String,
}

/// A link found in a stored conversation, with the context around it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredLink {
    pub link: LinkRef,
    /// Slack timestamp of the message that holds the link.
    pub message_ts: String,
    /// The excerpt that becomes `referenced_in` (ADR-0023).
    pub excerpt: String,
}

/// SharePoint Online and OneDrive hosts. Sovereign clouds (`.sharepoint.cn`, ...)
/// and SharePoint Server are not supported (ADR-0021).
pub fn is_microsoft_host(host: &str) -> bool {
    host.ends_with(".sharepoint.com") || host == "onedrive.live.com" || host == "1drv.ms"
}

/// Classify a URL found in a message. `None` for everything that is not a
/// Google Drive file link or a SharePoint/OneDrive link (web pages, folders,
/// published documents, forms, Slack permalinks, ...).
pub fn classify_link(raw: &str) -> Option<LinkRef> {
    let mut u = url::Url::parse(raw.trim()).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    u.set_fragment(None);
    let host = u.host_str()?.to_ascii_lowercase();
    if host == "docs.google.com" || host == "drive.google.com" {
        let id = google_file_id(&u, &host)?;
        return Some(LinkRef {
            kind: SourceKind::GoogleDoc,
            key: format!("google:{id}"),
            url: u.to_string(),
            host,
        });
    }
    if is_microsoft_host(&host) {
        let url = u.to_string();
        let digest = Sha256::digest(url.as_bytes());
        return Some(LinkRef {
            kind: SourceKind::MicrosoftFile,
            key: format!("ms:{}", hex::encode(digest)),
            url,
            host,
        });
    }
    None
}

fn is_id(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The Drive file ID of a Docs, Sheets, Slides or Drive file link.
fn google_file_id(u: &url::Url, host: &str) -> Option<String> {
    let mut segs: Vec<&str> = u.path_segments()?.filter(|s| !s.is_empty()).collect();
    // `/a/<domain>/...` (Workspace) prefix.
    if segs.first() == Some(&"a") && segs.len() > 2 {
        segs.drain(..2);
    }
    match segs.first().copied()? {
        "open" | "uc" if host == "drive.google.com" => u
            .query_pairs()
            .find(|(k, _)| k == "id")
            .map(|(_, v)| v.into_owned())
            .filter(|v| is_id(v)),
        "document" | "spreadsheets" | "presentation" | "file" => {
            // `/<kind>/[u/<n>/]d/<id>`; `/d/e/<token>` is published to the web.
            let mut rest = &segs[1..];
            if rest.first() == Some(&"u") && rest.len() > 2 {
                rest = &rest[2..];
            }
            match rest {
                ["d", "e", ..] => None,
                ["d", id, ..] if is_id(id) => Some((*id).to_string()),
                _ => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(u: &str) -> Option<String> {
        classify_link(u).map(|l| l.key)
    }

    #[test]
    fn google_forms_of_one_file_share_a_key() {
        let want = Some("google:1AbC_d-9".to_string());
        for u in [
            "https://docs.google.com/document/d/1AbC_d-9/edit?usp=sharing",
            "https://docs.google.com/document/u/0/d/1AbC_d-9/edit",
            "https://docs.google.com/a/example.org/document/d/1AbC_d-9/edit",
            "https://docs.google.com/spreadsheets/d/1AbC_d-9/edit#gid=0",
            "https://docs.google.com/presentation/d/1AbC_d-9/edit",
            "https://drive.google.com/file/d/1AbC_d-9/view",
            "https://drive.google.com/open?id=1AbC_d-9",
            "https://drive.google.com/uc?id=1AbC_d-9&export=download",
        ] {
            assert_eq!(key(u), want, "{u}");
        }
    }

    #[test]
    fn things_that_are_not_file_links_are_ignored() {
        for u in [
            "https://docs.google.com/document/d/e/2PACX-abc/pub",
            "https://docs.google.com/forms/d/1AbC/edit",
            "https://drive.google.com/drive/folders/1AbC",
            "https://drive.google.com/drive/u/0/my-drive",
            "https://example.com/document/d/1AbC/edit",
            "https://myslack.slack.com/archives/C1/p1",
            "https://example.com/page",
            "ftp://docs.google.com/document/d/1AbC",
            "not a url",
        ] {
            assert_eq!(key(u), None, "{u}");
        }
    }

    #[test]
    fn microsoft_links_are_keyed_by_the_url_without_fragment() {
        let a =
            classify_link("https://contoso.sharepoint.com/:w:/s/team/EabcDEF?e=xyz#top").unwrap();
        let b = classify_link("https://contoso.sharepoint.com/:w:/s/team/EabcDEF?e=xyz").unwrap();
        assert_eq!(a.kind, SourceKind::MicrosoftFile);
        assert_eq!(a.key, b.key);
        assert!(a.key.starts_with("ms:"));
        assert_eq!(a.host, "contoso.sharepoint.com");
        assert!(!a.url.contains('#'));
        assert!(classify_link("https://1drv.ms/w/s!Abc").is_some());
        assert!(classify_link("https://onedrive.live.com/redir?resid=1").is_some());
        assert!(classify_link("https://sharepoint.com.example.org/a").is_none());
        assert!(classify_link("https://notsharepoint.com/a").is_none());
    }
}
