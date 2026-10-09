//! H1 for the observed 2.28.0 list-models JSON. Descriptions, display names,
//! prices and the default model are not aliases or effort-capability evidence.
use std::collections::BTreeSet;

use serde::Deserialize;

use super::{native::validate_catalog_request, KiroAdapter};
use crate::contract::{hooks::*, plan::*, types::*};

const EFFORT_UNKNOWN: Reason = Reason {
    code: "kiro-catalog-effort-not-reported",
    message: "This catalog does not report per-model effort support; CLI flag syntax is not model capability evidence",
};

#[derive(Deserialize)]
struct Catalog {
    models: Vec<Model>,
    default_model: String,
}
#[derive(Deserialize)]
struct Model {
    model_id: String,
    model_name: String,
    context_window_tokens: u64,
}

impl CatalogHook for KiroAdapter {
    fn discover(
        &self,
        request: &CatalogRequest,
        host: &mut dyn BoundedReadHost,
    ) -> Result<CatalogObservation, ReadFailure> {
        validate_catalog_request(request).map_err(ReadFailure::Error)?;
        let output = host.read_catalog(request)?;
        if output.elapsed > request.bounds.deadline {
            return Err(ReadFailure::TimedOut { elapsed: output.elapsed });
        }
        if output.stdout.len() > request.bounds.max_output_bytes {
            return Err(ReadFailure::OutputLimit { limit: request.bounds.max_output_bytes });
        }
        if output.exit_code != 0 {
            return Err(ReadFailure::Exit { code: output.exit_code });
        }
        // Deserialize directly into the observed schema: duplicate authority
        // keys are errors, not last-wins Values. Unknown metadata is not promoted.
        let catalog: Catalog = serde_json::from_slice(&output.stdout)
            .map_err(|_| ReadFailure::Error(ContractError::Invalid("Kiro model catalog JSON")))?;
        if catalog.models.is_empty() || !nonblank(&catalog.default_model) {
            return Err(ReadFailure::Error(ContractError::Invalid("Kiro model catalog default")));
        }
        let mut ids = BTreeSet::new();
        for model in &catalog.models {
            if !nonblank(&model.model_id) || !nonblank(&model.model_name) || model.context_window_tokens == 0 {
                return Err(ReadFailure::Error(ContractError::Invalid("Kiro model record")));
            }
            if !ids.insert(model.model_id.as_str()) {
                return Err(ReadFailure::Error(ContractError::AmbiguousModel));
            }
        }
        if !ids.contains(catalog.default_model.as_str()) {
            return Err(ReadFailure::Error(ContractError::Invalid("Kiro model catalog default")));
        }
        Ok(CatalogObservation {
            provider: request.provider.clone(),
            native: request.native.clone(),
            schema: request.source.schema.into(),
            models: catalog.models.into_iter().map(|model| ModelRecord {
                id: model.model_id,
                efforts: Support::Unverified(EFFORT_UNKNOWN),
            }).collect(),
        })
    }
}
