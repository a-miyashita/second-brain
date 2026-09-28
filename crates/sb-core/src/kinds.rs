//! String-backed enums shared by the catalog, the CLI and the JSON contract.

use std::fmt;
use std::str::FromStr;

/// Error returned when parsing an unknown enum value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown {what}: {value:?}")]
pub struct ParseKindError {
    pub what: &'static str,
    pub value: String,
}

macro_rules! str_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident ($what:literal) {
            $( $(#[$vmeta:meta])* $variant:ident => $s:literal ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        $vis enum $name {
            $( $(#[$vmeta])* $variant ),+
        }

        impl $name {
            /// All variants, in declaration order.
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// The stable string key.
            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $s),+
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = ParseKindError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($s => Ok($name::$variant),)+
                    _ => Err(ParseKindError { what: $what, value: s.to_string() }),
                }
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                s.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

str_enum! {
    /// Kind of account (ADR-0007).
    pub enum AccountKind ("account kind") {
        Google => "google",
        Slack => "slack",
        Imap => "imap",
        Local => "local",
        Web => "web",
    }
}

str_enum! {
    /// Kind of source an entry comes from (ADR-0008).
    pub enum SourceKind ("source kind") {
        SlackThread => "slack.thread",
        SlackDay => "slack.day",
        GoogleMeet => "google.meet",
        GoogleDoc => "google.doc",
        MailMessage => "mail.message",
        MailThread => "mail.thread",
        WebPage => "web.page",
        LocalFile => "local.file",
    }
}

impl SourceKind {
    /// The account kind that owns entries of this source kind.
    pub fn account_kind(self) -> AccountKind {
        match self {
            SourceKind::SlackThread | SourceKind::SlackDay => AccountKind::Slack,
            SourceKind::GoogleMeet | SourceKind::GoogleDoc => AccountKind::Google,
            SourceKind::MailMessage | SourceKind::MailThread => AccountKind::Imap,
            SourceKind::WebPage => AccountKind::Web,
            SourceKind::LocalFile => AccountKind::Local,
        }
    }
}

str_enum! {
    /// Kind of an entry section.
    pub enum SectionKind ("section kind") {
        Overview => "overview",
        Decisions => "decisions",
        ActionItems => "action_items",
        Details => "details",
        Background => "background",
    }
}

impl SectionKind {
    /// Default display position of the section within an entry.
    pub fn position(self) -> i64 {
        match self {
            SectionKind::Background => 0,
            SectionKind::Overview => 1,
            SectionKind::Decisions => 2,
            SectionKind::ActionItems => 3,
            SectionKind::Details => 4,
        }
    }

    /// Localized label for human-readable output.
    pub fn label(self, lang: Language) -> &'static str {
        match (lang, self) {
            (Language::En, SectionKind::Overview) => "Overview",
            (Language::En, SectionKind::Decisions) => "Decisions",
            (Language::En, SectionKind::ActionItems) => "Action items",
            (Language::En, SectionKind::Details) => "Details",
            (Language::En, SectionKind::Background) => "Background",
            (Language::Ja, SectionKind::Overview) => "概要",
            (Language::Ja, SectionKind::Decisions) => "決定事項",
            (Language::Ja, SectionKind::ActionItems) => "アクションアイテム",
            (Language::Ja, SectionKind::Details) => "詳細",
            (Language::Ja, SectionKind::Background) => "背景",
        }
    }
}

str_enum! {
    /// Who produced a section (ADR-0005).
    pub enum SectionOrigin ("section origin") {
        Generated => "generated",
        Extracted => "extracted",
        User => "user",
    }
}

str_enum! {
    /// Role of a raw object within an entry's raw bundle.
    pub enum RawRole ("raw role") {
        Primary => "primary",
        Notes => "notes",
        Transcript => "transcript",
        Attachment => "attachment",
        ExtractedText => "extracted_text",
    }
}

str_enum! {
    /// Availability of an entry's raw data (ADR-0002).
    pub enum RawStatus ("raw status") {
        Present => "present",
        Missing => "missing",
        FetchFailed => "fetch_failed",
    }
}

str_enum! {
    /// Summarization state of an entry.
    pub enum SummaryStatus ("summary status") {
        None => "none",
        Pending => "pending",
        Done => "done",
        Skipped => "skipped",
        Failed => "failed",
    }
}

str_enum! {
    /// How an entry first entered the catalog.
    pub enum EntryOrigin ("entry origin") {
        Sync => "sync",
        Ingest => "ingest",
        Import => "import",
    }
}

str_enum! {
    /// Kind of generator that produced a summary (ADR-0005).
    pub enum GeneratorKind ("generator kind") {
        LlmApi => "llm_api",
        LlmCli => "llm_cli",
        LocalLlm => "local_llm",
        SourceNative => "source_native",
        Unknown => "unknown",
    }
}

str_enum! {
    /// Status of an account.
    pub enum AccountStatus ("account status") {
        Active => "active",
        Disabled => "disabled",
        NeedsReauth => "needs_reauth",
    }
}

str_enum! {
    /// Final or current status of a run.
    pub enum RunStatus ("run status") {
        Running => "running",
        Ok => "ok",
        Partial => "partial",
        Failed => "failed",
        Interrupted => "interrupted",
        StoppedByLimit => "stopped_by_limit",
    }
}

str_enum! {
    /// What started a run.
    pub enum RunTrigger ("run trigger") {
        Manual => "manual",
        Schedule => "schedule",
        Mcp => "mcp",
    }
}

str_enum! {
    /// Severity of an issue or a doctor check.
    pub enum Severity ("severity") {
        Info => "info",
        Warning => "warning",
        Error => "error",
    }
}

str_enum! {
    /// Display language for human-readable labels.
    pub enum Language ("language") {
        Ja => "ja",
        En => "en",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_all_source_kinds() {
        for k in SourceKind::ALL {
            assert_eq!(k.as_str().parse::<SourceKind>(), Ok(*k));
        }
    }

    #[test]
    fn unknown_value_is_an_error() {
        let err = "slack.channel".parse::<SourceKind>().unwrap_err();
        assert_eq!(err.what, "source kind");
    }

    #[test]
    fn serde_uses_string_keys() {
        let json = serde_json::to_string(&SectionKind::ActionItems).unwrap();
        assert_eq!(json, "\"action_items\"");
        let back: SectionKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, SectionKind::ActionItems);
    }
}
