use super::*;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct CatalogFile {
    pub(super) version: u32,
    pub(super) fetched_at_ms: u64,
    pub(super) models: Vec<OpenRouterModel>,
}

pub(super) fn validate_catalog(models: &[OpenRouterModel]) -> Result<(), OpenRouterStoreError> {
    if models.len() > MAX_CATALOG_MODELS {
        return Err(limit_error());
    }
    let mut ids = HashSet::with_capacity(models.len());
    let mut text = 0usize;
    for model in models {
        if !model.validate() || !ids.insert(&model.id) {
            return Err(corrupt_error());
        }
        text = text
            .saturating_add(model.id.len())
            .saturating_add(model.name.as_ref().map_or(0, String::len));
        if text > MAX_CATALOG_TEXT_BYTES {
            return Err(limit_error());
        }
    }
    Ok(())
}
