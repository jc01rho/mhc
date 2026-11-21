use maho_ai::auth::types::{AuthInteraction, AuthPrompt, AuthPromptKind, AuthPromptOption};
use maho_ai::utils::abort::AbortController;
use maho_ext_api::*;
use maho_ext_builtin_loose::oauth_login_interaction::ExtensionLoginInteraction;
use std::sync::Arc;

struct DialogUi {
    answer: Option<String>,
    supports_secret: bool,
    recorded_secret: std::sync::Mutex<Vec<bool>>,
    refuse_secret: bool,
}
impl ExtensionUi for DialogUi {
    fn supports_secret_input(&self) -> bool {
        self.supports_secret
    }
    fn input_secret<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a str>,
        opts: ExtensionUiDialogOptions,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        Option<String>,
                        maho_ext_api::secret_input::SecretInputUnavailableError,
                    >,
                > + Send
                + 'a,
        >,
    > {
        self.recorded_secret.lock().expect("lock").push(opts.secret);
        let refuse = self.refuse_secret;
        let answer = self.answer.clone();
        Box::pin(async move {
            if refuse {
                Err(
                    maho_ext_api::secret_input::SecretInputUnavailableError::new(
                        maho_ext_api::secret_input::SecretInputUnavailableReason::ClientUnsupported,
                    ),
                )
            } else {
                Ok(answer)
            }
        })
    }
    fn select<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { self.answer.clone() })
    }
    fn confirm<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, bool> {
        panic!("unexpected confirm")
    }
    fn input<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a str>,
        opts: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        self.recorded_secret.lock().expect("lock").push(opts.secret);
        Box::pin(async { self.answer.clone() })
    }
    fn notify(&self, _: &str, _: NotificationType) {
        panic!("unexpected notification")
    }
    fn set_status(&self, _: &str, _: Option<&str>) {
        panic!("unexpected status")
    }
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {
        panic!("unexpected widget")
    }
    fn set_header(&self, _: Option<ComponentFactory>) {
        panic!("unexpected header")
    }
    fn set_footer(&self, _: Option<ComponentFactory>) {
        panic!("unexpected footer")
    }
    fn set_title(&self, _: &str) {
        panic!("unexpected title")
    }
    fn paste_to_editor(&self, _: &str) {
        panic!("unexpected editor")
    }
    fn set_editor_text(&self, _: &str) {
        panic!("unexpected editor")
    }
    fn get_editor_text(&self) -> String {
        panic!("unexpected editor")
    }
    fn custom(
        &self,
        _: ComponentFactory,
        _: CustomUiOptions,
    ) -> ExtensionFuture<'_, serde_json::Value> {
        panic!("unexpected custom UI")
    }
    fn theme(&self) -> Theme {
        panic!("unexpected theme")
    }
}
fn interaction(answer: Option<&str>, provider: Option<&str>) -> ExtensionLoginInteraction {
    ExtensionLoginInteraction::new(
        Arc::new(DialogUi {
            answer: answer.map(str::to_owned),
            supports_secret: false,
            recorded_secret: Default::default(),
            refuse_secret: false,
        }),
        ExtensionMode::Rpc,
        "Test".into(),
        provider,
        Arc::new(|_| panic!("unexpected browser")),
    )
}
fn text_prompt() -> AuthPrompt {
    AuthPrompt {
        kind: AuthPromptKind::Text {
            message: "Code".into(),
            placeholder: None,
        },
        signal: None,
    }
}

#[tokio::test]
async fn select_maps_label_to_provider_option_id() {
    let login = interaction(Some("Visible label"), None);
    let prompt = AuthPrompt {
        kind: AuthPromptKind::Select {
            message: "Choose".into(),
            options: vec![AuthPromptOption {
                id: "machine-id".into(),
                label: "Visible label".into(),
                description: None,
            }],
        },
        signal: None,
    };
    let answer = login.prompt(prompt).await.expect("answer");
    assert_eq!(answer, "machine-id");
}

#[tokio::test]
async fn dismissing_owned_dialog_aborts_login() {
    let login = interaction(None, None);
    let result = login.prompt(text_prompt()).await;
    assert!(result.is_err());
    assert!(login.signal().expect("signal").aborted());
}

#[tokio::test]
async fn aborted_prompt_does_not_abort_login() {
    let login = interaction(Some("code"), None);
    let prompt_controller = AbortController::new();
    prompt_controller.abort(None);
    let mut prompt = text_prompt();
    prompt.signal = Some(prompt_controller.signal());
    let result = login.prompt(prompt).await;
    assert!(result.is_err());
    assert!(!login.signal().expect("signal").aborted());
}

#[test]
fn reissuing_provider_login_cancels_previous_only() {
    let previous = interaction(Some("code"), Some("oauth-test-reissue"));
    let unrelated = interaction(Some("code"), Some("oauth-test-unrelated"));
    let current = interaction(Some("code"), Some("oauth-test-reissue"));
    assert!(previous.signal().expect("signal").aborted());
    assert!(!unrelated.signal().expect("signal").aborted());
    assert!(!current.signal().expect("signal").aborted());
}

#[tokio::test]
async fn secret_prompt_uses_secret_option_only_when_ui_supports_it() {
    for supported in [false, true] {
        let ui = Arc::new(DialogUi {
            answer: Some("synthetic".into()),
            supports_secret: supported,
            recorded_secret: Default::default(),
            refuse_secret: false,
        });
        let login = ExtensionLoginInteraction::new(
            ui.clone(),
            ExtensionMode::Rpc,
            "Test".into(),
            None,
            Arc::new(|_| panic!("unexpected browser")),
        );
        let answer = login
            .prompt(AuthPrompt {
                kind: AuthPromptKind::Secret {
                    message: "Credential".into(),
                    placeholder: Some("hint".into()),
                },
                signal: None,
            })
            .await
            .expect("answer");
        assert_eq!(answer, "synthetic");
        assert_eq!(
            ui.recorded_secret.lock().expect("lock").as_slice(),
            &[supported]
        );
    }

    let refusing_ui = Arc::new(DialogUi {
        answer: None,
        supports_secret: true,
        recorded_secret: Default::default(),
        refuse_secret: true,
    });
    let login = ExtensionLoginInteraction::new(
        refusing_ui,
        ExtensionMode::Rpc,
        "Test".into(),
        None,
        Arc::new(|_| panic!("unexpected browser")),
    );
    let err = login
        .prompt(AuthPrompt {
            kind: AuthPromptKind::Secret {
                message: "Credential".into(),
                placeholder: None,
            },
            signal: None,
        })
        .await
        .expect_err("refusal");
    assert!(
        err.to_string()
            .contains("use the TUI or the host's settings route")
    );
}
