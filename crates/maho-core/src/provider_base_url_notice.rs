use crate::model_config_schema::ModelsJsonProvider;

const FIXED_ADDRESS_PROVIDERS: [(&str, &str); 1] = [("cursor-cli-oauth", "Cursor")];

fn sets_base_url(config: &ModelsJsonProvider) -> bool {
    config.base_url.is_some()
        || config
            .models
            .as_ref()
            .is_some_and(|models| models.iter().any(|model| model.base_url.is_some()))
}

pub fn fixed_address_base_url_notices(
    providers: &indexmap::IndexMap<String, ModelsJsonProvider>,
) -> Vec<String> {
    let mut notices = Vec::new();
    for (provider_id, service) in FIXED_ADDRESS_PROVIDERS {
        let Some(config) = providers.get(provider_id) else {
            continue;
        };
        if !sets_base_url(config) {
            continue;
        }
        notices.push(format!(
            "models.json sets a baseUrl for {provider_id}, but {service} does not support changing the address. Requests still go to {service}."
        ));
    }
    notices
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(base_url: Option<&str>) -> ModelsJsonProvider {
        ModelsJsonProvider {
            base_url: base_url.map(str::to_owned),
            ..Default::default()
        }
    }

    #[test]
    fn only_fixed_address_providers_with_a_base_url_get_a_notice() {
        let mut providers = indexmap::IndexMap::new();
        providers.insert(
            "cursor-cli-oauth".to_owned(),
            provider(Some("http://proxy")),
        );
        providers.insert("proxy".to_owned(), provider(Some("http://proxy")));
        let notices = fixed_address_base_url_notices(&providers);
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("cursor-cli-oauth"));
        assert!(notices[0].contains("Requests still go to Cursor."));

        let mut without = indexmap::IndexMap::new();
        without.insert("cursor-cli-oauth".to_owned(), provider(None));
        assert!(fixed_address_base_url_notices(&without).is_empty());
    }
}
