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

pub fn normalize_presentation(value: Option<&Value>) -> (PresentationRequest, Option<String>) {
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
}
