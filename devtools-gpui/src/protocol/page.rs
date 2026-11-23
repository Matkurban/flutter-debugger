use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum DevToolsPage {
    Inspector,
    #[default]
    Network,
    Logging,
    Performance,
    Memory,
}

impl DevToolsPage {
    pub fn parse(input: &str) -> Option<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "inspector" | "widget" | "widgets" => Some(Self::Inspector),
            "network" | "net" | "http" => Some(Self::Network),
            "logging" | "logs" | "log" => Some(Self::Logging),
            "performance" | "perf" | "frames" => Some(Self::Performance),
            "memory" | "mem" | "heap" => Some(Self::Memory),
            _ => None,
        }
    }

    pub fn as_slug(self) -> &'static str {
        match self {
            Self::Inspector => "inspector",
            Self::Network => "network",
            Self::Logging => "logging",
            Self::Performance => "performance",
            Self::Memory => "memory",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Inspector => "Inspector",
            Self::Network => "Network",
            Self::Logging => "Logging",
            Self::Performance => "Performance",
            Self::Memory => "Memory",
        }
    }

    pub fn icon_name(self) -> gpui_kit::assets::IconName {
        use gpui_kit::assets::IconName;
        match self {
            Self::Inspector => IconName::LayoutGrid,
            Self::Network => IconName::Network,
            Self::Logging => IconName::SquareTerminal,
            Self::Performance => IconName::Activity,
            Self::Memory => IconName::MemoryStick,
        }
    }

    pub const fn all() -> [Self; 5] {
        [
            Self::Inspector,
            Self::Network,
            Self::Logging,
            Self::Performance,
            Self::Memory,
        ]
    }
}
