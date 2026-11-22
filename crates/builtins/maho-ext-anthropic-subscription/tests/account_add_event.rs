use maho_ai::auth::{
    credential_store::InMemoryCredentialStore,
    types::{CredentialStore, ModelAuth, OAuthAuth, OAuthCredential, ProviderAuthInteraction},
};
use maho_ext_anthropic_subscription::{
    account_events::{ProviderAccountEvent, provider_account_events},
    accounts::list_accounts,
    availability::AmbientAuthStatusReader,
    oauth_login::AnthropicSubscriptionOAuth,
};
use maho_ext_host::loader::NativeExtensionFactory;
use maho_test_support::{faux::FauxScript, faux_session::FauxSession};
use std::sync::{Arc, Mutex};

struct StubLogin;

#[async_trait::async_trait]
impl OAuthAuth for StubLogin {
    fn name(&self) -> &str {
        "synthetic"
    }
    async fn login(
        &self,
        interaction: &ProviderAuthInteraction,
    ) -> anyhow::Result<OAuthCredential> {
        interaction.signal.throw_if_aborted()?;
        Ok(OAuthCredential::new("fresh", "refresh", 60_000.0))
    }
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        _: &maho_ai::utils::abort::AbortSignal,
    ) -> anyhow::Result<OAuthCredential> {
        Ok(credential.clone())
    }
    async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            ..Default::default()
        })
    }
}

struct AddAccount {
    store: Arc<InMemoryCredentialStore>,
    events: Arc<Mutex<Vec<ProviderAccountEvent>>>,
}

impl maho_ext_api::Extension for AddAccount {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        let settings = Arc::new(|| {
            maho_ext_anthropic_subscription::settings::load(
                &serde_json::json!({}),
                &serde_json::Value::Null,
                &Default::default(),
            )
        });
        let oauth = Arc::new(AnthropicSubscriptionOAuth {
            store: self.store.clone(),
            flow: Arc::new(StubLogin),
            settings,
            ambient: AmbientAuthStatusReader::new(
                Arc::new(|| Box::pin(async { Ok(false) })),
                Arc::new(|| 1),
                30_000,
            ),
        });
        maho_ext_anthropic_subscription::account_command::register(
            api,
            oauth,
            Arc::new(Default::default()),
        );
        let handler = api
            .registered
            .commands
            .iter()
            .find(|command| command.name == "claude-account")
            .expect("command")
            .handler
            .clone();
        let events = self.events.clone();
        api.register_command(
            "assert-add-single-event",
            None,
            None,
            Arc::new(move |_, ctx| {
                let handler = handler.clone();
                let events = events.clone();
                Box::pin(async move {
                    let unsubscribe = provider_account_events().subscribe(Arc::new(
                        move |event: &ProviderAccountEvent| {
                            events.lock().expect("events").push(event.clone());
                        },
                    ));
                    let mut interactive = ctx.clone();
                    interactive.has_ui = true;
                    let outcome = handler("add", &interactive).await;
                    unsubscribe();
                    outcome
                })
            }),
        );
    }
}

#[tokio::test]
async fn one_account_add_emits_accounts_changed_exactly_once() {
    let store = Arc::new(InMemoryCredentialStore::new());
    let events = Arc::new(Mutex::new(Vec::new()));
    let session = FauxSession::new(FauxScript {
        name: "account-add-event".into(),
        prompt: "/assert-add-single-event".into(),
        responses: Vec::new(),
    })
    .with_native_extension(NativeExtensionFactory {
        path: "<anthropic-subscription>".into(),
        source_info: Default::default(),
        extension: Box::new(AddAccount {
            store: store.clone(),
            events: events.clone(),
        }),
    });

    tokio::time::timeout(std::time::Duration::from_secs(15), session.run_native())
        .await
        .expect("bounded scenario")
        .expect("scenario");

    let credential = store
        .read("anthropic-subscription", None)
        .await
        .expect("read")
        .expect("credential")
        .into_oauth()
        .expect("oauth");
    let names: Vec<String> = list_accounts(&credential, None)
        .expect("slots")
        .into_iter()
        .map(|slot| slot.name)
        .collect();
    assert_eq!(names, ["default"]);
    let events = events.lock().expect("events");
    let changed = events
        .iter()
        .filter(|event| {
            matches!(event, ProviderAccountEvent::AccountsChanged { provider } if provider == "anthropic-subscription")
        })
        .count();
    assert_eq!(changed, 1, "{events:?}");
}
