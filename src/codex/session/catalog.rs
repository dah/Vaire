use super::*;

impl SessionService {
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, SessionError> {
        let mut cursor = None;
        let mut seen_cursors = HashSet::new();
        let mut seen_models = HashSet::new();
        let mut models = Vec::new();
        let mut pages = 0;
        let mut budget = PaginationBudget::default();
        loop {
            let response: ModelListResponse = decode(
                "model/list",
                self.transport
                    .request_default(
                        "model/list",
                        ModelListParams {
                            cursor: cursor.clone(),
                            include_hidden: false,
                        },
                    )
                    .await?,
            )?;
            pages += 1;
            validate_page_len("model/list", response.data.len(), MAX_MODEL_PAGE_ITEMS)?;
            for model in response.data {
                if model.hidden {
                    continue;
                }
                if !valid_identifier(&model.id)
                    || !valid_identifier(&model.default_reasoning_effort)
                    || model
                        .supported_reasoning_efforts
                        .iter()
                        .any(|option| !valid_identifier(&option.reasoning_effort))
                {
                    return Err(SessionError::Protocol(
                        "model/list returned an invalid model or reasoning id".to_owned(),
                    ));
                }
                if seen_models.insert(model.id.clone()) {
                    budget.retain("model/list", model_retained_bytes(&model))?;
                    models.push(model);
                }
            }
            let Some(next) =
                next_cursor("model/list", pages, &mut seen_cursors, response.next_cursor)?
            else {
                break;
            };
            cursor = Some(next);
        }
        Ok(models)
    }
}
