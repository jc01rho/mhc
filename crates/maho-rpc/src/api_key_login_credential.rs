use async_trait::async_trait;
use maho_ai::{
    auth::types::{
        ApiKeyCredential, AuthEvent, AuthInteraction, AuthPrompt, AuthPromptKind,
        ProviderAuthInteraction,
    },
    models::Provider,
    utils::abort::{AbortController, AbortSignal},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const REGION_LOGIN_TIMEOUT_MS: u64 = 15_000;

struct RegionLoginInteraction {
    signal: AbortSignal,
    provider_name: String,
    key: String,
    region: String,
    region_asked: Arc<AtomicBool>,
}

#[async_trait]
impl AuthInteraction for RegionLoginInteraction {
    fn signal(&self) -> Option<AbortSignal> {
        Some(self.signal.clone())
    }

    async fn prompt(&self, prompt: AuthPrompt) -> anyhow::Result<String> {
        match prompt.kind {
            AuthPromptKind::Secret { .. } => Ok(self.key.clone()),
            AuthPromptKind::Select { options, .. } if !self.region_asked.load(Ordering::SeqCst) => {
                if !options.iter().any(|option| option.id == self.region) {
                    anyhow::bail!(
                        "Unknown region \"{}\" for {}.",
                        self.region,
                        self.provider_name
                    );
                }
                self.region_asked.store(true, Ordering::SeqCst);
                Ok(self.region.clone())
            }
            _ => anyhow::bail!(
                "{} asked for more than a key and a region.",
                self.provider_name
            ),
        }
    }

    fn notify(&self, _event: AuthEvent) {}
}

pub async fn api_key_login_credential(
    provider: Option<&dyn Provider>,
    key: &str,
    region: Option<&str>,
) -> Result<ApiKeyCredential, String> {
    let Some(region) = region else {
        return Ok(ApiKeyCredential {
            key: Some(key.to_owned()),
            env: None,
            extra: Default::default(),
        });
    };
    let provider_name = provider.map_or("This provider", Provider::name);
    let Some(api_key) = provider
        .and_then(Provider::auth)
        .and_then(|auth| auth.api_key.as_ref())
        .filter(|api_key| api_key.has_login())
    else {
        return Err(format!(
            "{provider_name} does not take a region for its API key."
        ));
    };
    let controller = AbortController::new();
    let signal = controller.signal();
    let region_asked = Arc::new(AtomicBool::new(false));
    let interaction = ProviderAuthInteraction::new(
        signal.clone(),
        Arc::new(RegionLoginInteraction {
            signal,
            provider_name: provider_name.to_owned(),
            key: key.to_owned(),
            region: region.to_owned(),
            region_asked: region_asked.clone(),
        }),
    );
    let mut credential = tokio::time::timeout(
        Duration::from_millis(REGION_LOGIN_TIMEOUT_MS),
        api_key.login(&interaction),
    )
    .await
    .map_err(|_| {
        controller.abort(None);
        format!("{provider_name} API key login timed out.")
    })?
    .map_err(|error| error.to_string())?;
    if !region_asked.load(Ordering::SeqCst) {
        let Some(env) = credential.env.as_ref() else {
            return Err(format!(
                "{provider_name} does not take a region for its API key."
            ));
        };
        if !env.values().any(|value| value == region) {
            return Err(format!(
                "This host's environment sets the {provider_name} region, so \"{region}\" cannot be chosen here."
            ));
        }
    }
    credential.key = Some(key.to_owned());
    Ok(credential)
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ai::{
        auth::types::{ApiKeyAuth, AuthContext, AuthPromptOption, AuthResult, ProviderAuth},
        models::Model,
        types::ProviderEnv,
    };

    struct SelectsApiKey {
        selects: usize,
        preset_env: Option<ProviderEnv>,
    }

    #[async_trait]
    impl ApiKeyAuth for SelectsApiKey {
        fn name(&self) -> &str {
            "Two Questions key"
        }

        fn has_login(&self) -> bool {
            true
        }

        async fn login(
            &self,
            interaction: &ProviderAuthInteraction,
        ) -> anyhow::Result<ApiKeyCredential> {
            let mut answers = Vec::new();
            for index in 0..self.selects {
                answers.push(
                    interaction
                        .prompt(AuthPrompt {
                            kind: AuthPromptKind::Select {
                                message: format!("question {index}"),
                                options: vec![AuthPromptOption {
                                    id: "global".into(),
                                    label: "Global".into(),
                                    description: None,
                                }],
                            },
                            signal: Some(interaction.signal.clone()),
                        })
                        .await?,
                );
            }
            let key = interaction
                .prompt(AuthPrompt {
                    kind: AuthPromptKind::Secret {
                        message: "key".into(),
                        placeholder: None,
                    },
                    signal: Some(interaction.signal.clone()),
                })
                .await?;
            let env = if let Some(preset) = &self.preset_env {
                Some(preset.clone())
            } else if answers.is_empty() {
                None
            } else {
                Some(ProviderEnv::from([(
                    "ANSWERS".to_owned(),
                    answers.join(","),
                )]))
            };
            Ok(ApiKeyCredential {
                key: Some(key),
                env,
                extra: Default::default(),
            })
        }

        async fn resolve(
            &self,
            _ctx: &dyn AuthContext,
            _credential: Option<&ApiKeyCredential>,
            _signal: &AbortSignal,
        ) -> anyhow::Result<Option<AuthResult>> {
            Ok(None)
        }
    }

    struct FakeProvider {
        auth: Option<ProviderAuth>,
    }

    impl Provider for FakeProvider {
        fn id(&self) -> &str {
            "two-questions"
        }

        fn name(&self) -> &str {
            "Two Questions"
        }

        fn auth(&self) -> Option<&ProviderAuth> {
            self.auth.as_ref()
        }

        fn get_models(&self) -> Vec<Model> {
            Vec::new()
        }
    }

    fn provider_asking(selects: usize, preset_env: Option<ProviderEnv>) -> FakeProvider {
        FakeProvider {
            auth: Some(ProviderAuth {
                api_key: Some(Arc::new(SelectsApiKey {
                    selects,
                    preset_env,
                })),
                oauth: None,
            }),
        }
    }

    #[tokio::test]
    async fn answers_the_region_to_the_first_select_only_and_refuses_a_second_one() {
        let one = provider_asking(1, None);
        let cred = api_key_login_credential(Some(&one), "sk-one", Some("global"))
            .await
            .expect("one select");
        assert_eq!(cred.key.as_deref(), Some("sk-one"));
        assert_eq!(
            cred.env,
            Some(ProviderEnv::from([(
                "ANSWERS".to_owned(),
                "global".to_owned()
            )]))
        );

        let two = provider_asking(2, None);
        let err = api_key_login_credential(Some(&two), "sk-two", Some("global"))
            .await
            .expect_err("second select refused");
        assert!(
            err.contains("asked for more than a key and a region"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn refuses_unknown_region_and_respects_env_pinned_region() {
        let one = provider_asking(1, None);
        let err = api_key_login_credential(Some(&one), "sk-one", Some("moon"))
            .await
            .expect_err("unknown region");
        assert!(err.contains("Unknown region \"moon\""), "{err}");

        let pinned = provider_asking(
            0,
            Some(ProviderEnv::from([(
                "KIMI_CODE_REGION".to_owned(),
                "mainland-cn".to_owned(),
            )])),
        );
        let mismatch = api_key_login_credential(Some(&pinned), "sk-one", Some("global"))
            .await
            .expect_err("env mismatch");
        assert!(
            mismatch.contains("environment sets the Two Questions region"),
            "{mismatch}"
        );
        let matched = api_key_login_credential(Some(&pinned), "sk-one", Some("mainland-cn"))
            .await
            .expect("env matched");
        assert_eq!(matched.key.as_deref(), Some("sk-one"));
    }
}
