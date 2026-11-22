//! Port of senpi `packages/coding-agent/src/core/dynamic-prompt/types.ts`.

/// `AvailableTool["category"]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCategory {
    Search,
    Session,
    Command,
    Other,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PromptSurface {
    #[default]
    Terminal,
    App,
    Chat,
}

/// `TerminalOrApp`: `Exclude<PromptSurface, "chat">`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TerminalOrApp {
    #[default]
    Terminal,
    App,
}

/// `terminalOrApp(surface)`: chat takes the app wording where the two share a rule.
pub const fn terminal_or_app(surface: PromptSurface) -> TerminalOrApp {
    match surface {
        PromptSurface::Terminal => TerminalOrApp::Terminal,
        PromptSurface::App | PromptSurface::Chat => TerminalOrApp::App,
    }
}

/// Which wording the shared prompt generators render (senpi#5665): `Full` is their text from before the
/// senpi#5335 diet, `Diet` the slimmed text. A prompt preset declares it; every other prompt renders `Diet`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SharedSections {
    #[default]
    Diet,
    Full,
}

impl SharedSections {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Diet => "diet",
        }
    }
}

/// `AvailableTool`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableTool {
    pub name: String,
    pub category: ToolCategory,
}
