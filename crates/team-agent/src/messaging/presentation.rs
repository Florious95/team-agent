//!
//! Typed presentation metadata shared by message and result ingress.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresentationSink {
    Leader,
    Casefile,
    Silent,
}

impl PresentationSink {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Leader => "leader",
            Self::Casefile => "casefile",
            Self::Silent => "silent",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "leader" => Some(Self::Leader),
            "casefile" => Some(Self::Casefile),
            "silent" => Some(Self::Silent),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresentationClass {
    Message,
    Progress,
    StageResult,
    StagePass,
    Bounce,
    Blocking,
    FinalReview,
    Timeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationSource {
    Send,
    ReportResult,
}

impl PresentationClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Progress => "progress",
            Self::StageResult => "stage_result",
            Self::StagePass => "stage_pass",
            Self::Bounce => "bounce",
            Self::Blocking => "blocking",
            Self::FinalReview => "final_review",
            Self::Timeout => "timeout",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "message" => Some(Self::Message),
            "progress" => Some(Self::Progress),
            "stage_result" => Some(Self::StageResult),
            "stage_pass" => Some(Self::StagePass),
            "bounce" => Some(Self::Bounce),
            "blocking" => Some(Self::Blocking),
            "final_review" => Some(Self::FinalReview),
            "timeout" => Some(Self::Timeout),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresentationRequest {
    pub sink: PresentationSink,
    pub class: PresentationClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub case_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresentationDecision {
    pub sink: PresentationSink,
    pub class: PresentationClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub case_id: Option<String>,
    pub requested_sink: PresentationSink,
    pub effective_sink: PresentationSink,
    pub policy_reason: String,
    pub policy_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendPresentationInput {
    pub request: PresentationRequest,
    pub deprecation: Option<String>,
}

impl Default for PresentationRequest {
    fn default() -> Self {
        Self {
            sink: PresentationSink::Leader,
            class: PresentationClass::Message,
            case_id: None,
        }
    }
}

impl Default for PresentationDecision {
    fn default() -> Self {
        decide_presentation(
            &PresentationRequest::default(),
            PresentationSource::ReportResult,
        )
    }
}

pub fn decide_presentation(
    request: &PresentationRequest,
    source: PresentationSource,
) -> PresentationDecision {
    let critical = matches!(
        request.class,
        PresentationClass::StagePass
            | PresentationClass::Bounce
            | PresentationClass::Blocking
            | PresentationClass::FinalReview
            | PresentationClass::Timeout
    );
    let (effective_sink, policy_reason) = if critical {
        (
            PresentationSink::Leader,
            format!("critical_class:{}", request.class.as_str()),
        )
    } else if source == PresentationSource::ReportResult
        && request.class != PresentationClass::StageResult
    {
        (
            PresentationSink::Leader,
            format!("user_delivery_class:{}", request.class.as_str()),
        )
    } else {
        (
            request.sink,
            format!("requested_sink:{}", request.sink.as_str()),
        )
    };
    PresentationDecision {
        sink: request.sink,
        class: request.class,
        case_id: request.case_id.clone(),
        requested_sink: request.sink,
        effective_sink,
        policy_reason,
        policy_version: "team-presentation-v1".to_string(),
    }
}

pub fn normalize_report_presentation(
    value: Option<&Value>,
) -> (PresentationRequest, Option<String>) {
    let (request, error) = normalize_presentation(value);
    if error.is_some() {
        return (request, error);
    }
    let missing_case_id = request.class == PresentationClass::StageResult
        && request.sink != PresentationSink::Leader
        && request
            .case_id
            .as_deref()
            .is_none_or(|case_id| case_id.trim().is_empty());
    if missing_case_id {
        return (request, Some("missing_case_id".to_string()));
    }
    (request, None)
}

/// Normalize the one-bit send surface and the temporary S-011 compatibility
/// input into the existing presentation disposition primitive.
///
/// S-011 sensitive: if the compatibility period is ended by a hard-cut
/// decision, replace this table with one uniform legacy-input refusal.
pub fn normalize_send_presentation(
    mailbox: Option<&Value>,
    legacy_presentation: Option<&Value>,
) -> Result<SendPresentationInput, String> {
    if mailbox.is_some() && legacy_presentation.is_some() {
        return Err("mailbox_conflicts_with_deprecated_presentation".to_string());
    }
    if let Some(mailbox) = mailbox {
        let Some(mailbox) = mailbox.as_bool() else {
            return Err("mailbox_must_be_boolean".to_string());
        };
        return Ok(SendPresentationInput {
            request: PresentationRequest {
                sink: if mailbox {
                    PresentationSink::Casefile
                } else {
                    PresentationSink::Leader
                },
                class: PresentationClass::Message,
                case_id: None,
            },
            deprecation: None,
        });
    }
    let Some(legacy) = legacy_presentation else {
        return Ok(SendPresentationInput {
            request: PresentationRequest::default(),
            deprecation: None,
        });
    };
    let Some(object) = legacy.as_object() else {
        return Err("malformed_presentation".to_string());
    };
    let Some(class) = object.get("class").and_then(Value::as_str) else {
        return Err("missing_class".to_string());
    };
    let Some(class) = PresentationClass::parse(class) else {
        return Err(format!("unknown_class:{class}"));
    };
    let sink = match class {
        PresentationClass::Message | PresentationClass::Blocking | PresentationClass::Timeout => {
            PresentationSink::Leader
        }
        PresentationClass::Progress => PresentationSink::Casefile,
        PresentationClass::StageResult
        | PresentationClass::StagePass
        | PresentationClass::Bounce
        | PresentationClass::FinalReview => {
            return Err(format!(
                "deprecated_message_class:{}_requires_result_route",
                class.as_str()
            ));
        }
    };
    Ok(SendPresentationInput {
        request: PresentationRequest {
            sink,
            class: PresentationClass::Message,
            case_id: None,
        },
        deprecation: Some(format!(
            "message-class={} is deprecated; use mailbox={} instead",
            class.as_str(),
            sink != PresentationSink::Leader
        )),
    })
}

pub(crate) fn normalize_presentation(value: Option<&Value>) -> (PresentationRequest, Option<String>) {
    let Some(value) = value else {
        return (PresentationRequest::default(), None);
    };
    let Some(object) = value.as_object() else {
        return (
            PresentationRequest::default(),
            Some("malformed_presentation".to_string()),
        );
    };
    let Some(sink) = object.get("sink").and_then(Value::as_str) else {
        return (
            PresentationRequest::default(),
            Some("missing_sink".to_string()),
        );
    };
    let Some(sink) = PresentationSink::parse(sink) else {
        return (
            PresentationRequest::default(),
            Some(format!("unknown_sink:{sink}")),
        );
    };
    let Some(class) = object.get("class").and_then(Value::as_str) else {
        return (
            PresentationRequest::default(),
            Some("missing_class".to_string()),
        );
    };
    let Some(class) = PresentationClass::parse(class) else {
        return (
            PresentationRequest::default(),
            Some(format!("unknown_class:{class}")),
        );
    };
    let case_id = object
        .get("case_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    (
        PresentationRequest {
            sink,
            class,
            case_id,
        },
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_metadata_keeps_legacy_leader_default() {
        assert_eq!(
            normalize_presentation(None),
            (PresentationRequest::default(), None)
        );
    }

    #[test]
    fn malformed_or_unknown_metadata_is_observable() {
        assert_eq!(
            normalize_presentation(Some(&json!({"sink": "casefile"}))).1,
            Some("missing_class".to_string())
        );
        assert_eq!(
            normalize_presentation(Some(&json!({"sink": "bogus", "class": "message"}))).1,
            Some("unknown_sink:bogus".to_string())
        );
    }

    #[test]
    fn critical_classes_force_leader_while_prose_is_ignored() {
        let request = PresentationRequest {
            sink: PresentationSink::Casefile,
            class: PresentationClass::Blocking,
            case_id: None,
        };
        let decision = decide_presentation(&request, PresentationSource::ReportResult);
        assert_eq!(decision.effective_sink, PresentationSink::Leader);
        assert_eq!(decision.policy_reason, "critical_class:blocking");

        let benign = PresentationRequest {
            sink: PresentationSink::Casefile,
            class: PresentationClass::Message,
            case_id: None,
        };
        assert_eq!(
            decide_presentation(&benign, PresentationSource::Send).effective_sink,
            PresentationSink::Casefile
        );
        assert_eq!(
            decide_presentation(&benign, PresentationSource::ReportResult).effective_sink,
            PresentationSink::Leader
        );
    }

    #[test]
    fn report_stage_result_requires_case_id_only_for_non_leader_sink() {
        assert_eq!(
            normalize_report_presentation(Some(
                &json!({"sink": "casefile", "class": "stage_result"})
            ))
            .1,
            Some("missing_case_id".to_string())
        );
        assert_eq!(
            normalize_report_presentation(Some(&json!({
                "sink": "casefile",
                "class": "stage_result",
                "case_id": "case-1"
            })))
            .1,
            None
        );
        assert_eq!(
            normalize_presentation(Some(&json!({
                "sink": "casefile",
                "class": "stage_result"
            })))
            .1,
            None,
            "send normalization remains unchanged"
        );
    }

    #[test]
    fn send_mailbox_and_s011_compatibility_map_to_one_bit() {
        let mailbox = normalize_send_presentation(Some(&json!(true)), None).unwrap();
        assert_eq!(mailbox.request.sink, PresentationSink::Casefile);
        assert_eq!(mailbox.deprecation, None);

        for (class, sink) in [
            ("message", PresentationSink::Leader),
            ("progress", PresentationSink::Casefile),
            ("blocking", PresentationSink::Leader),
            ("timeout", PresentationSink::Leader),
        ] {
            let legacy = json!({"sink": "silent", "class": class});
            let mapped = normalize_send_presentation(None, Some(&legacy)).unwrap();
            assert_eq!(mapped.request.sink, sink, "{class}");
            assert!(mapped.deprecation.is_some(), "{class}");
        }
    }

    #[test]
    fn s011_stage_classes_require_result_route() {
        for class in ["stage_result", "stage_pass", "bounce", "final_review"] {
            let legacy = json!({"sink": "leader", "class": class});
            assert_eq!(
                normalize_send_presentation(None, Some(&legacy)).unwrap_err(),
                format!("deprecated_message_class:{class}_requires_result_route")
            );
        }
    }
}
