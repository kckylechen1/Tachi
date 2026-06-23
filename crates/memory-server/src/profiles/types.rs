#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolBundle {
    Observe,
    Remember,
    Coordinate,
    Operate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ToolProfile {
    pub(crate) observe: bool,
    pub(crate) remember: bool,
    pub(crate) coordinate: bool,
    pub(crate) operate: bool,
    pub(crate) admin: bool,
    /// Standard profile: curated 10-tool allow-list for IDE + CLI agents.
    /// When set, only patterns in STANDARD_MINIMAL_TOOL_PATTERNS pass.
    pub(crate) standard_minimal: bool,
    /// Delegate profile: curated 6-tool allow-list for worker agents.
    /// When set, only patterns in DELEGATE_MINIMAL_TOOL_PATTERNS pass.
    /// Standard takes precedence over delegate if both are set.
    pub(crate) delegate_minimal: bool,
}

impl ToolProfile {
    pub(crate) const fn observe() -> Self {
        Self {
            observe: true,
            remember: false,
            coordinate: false,
            operate: false,
            admin: false,
            standard_minimal: false,
            delegate_minimal: false,
        }
    }

    pub(crate) const fn remember() -> Self {
        Self {
            observe: true,
            remember: true,
            coordinate: false,
            operate: false,
            admin: false,
            standard_minimal: false,
            delegate_minimal: false,
        }
    }

    pub(crate) const fn coordinate() -> Self {
        Self {
            observe: true,
            remember: true,
            coordinate: true,
            operate: false,
            admin: false,
            standard_minimal: false,
            delegate_minimal: false,
        }
    }

    pub(crate) const fn operate() -> Self {
        Self {
            observe: true,
            remember: true,
            coordinate: false,
            operate: true,
            admin: false,
            standard_minimal: false,
            delegate_minimal: false,
        }
    }

    pub(crate) const fn admin() -> Self {
        Self {
            observe: true,
            remember: true,
            coordinate: true,
            operate: true,
            admin: true,
            standard_minimal: false,
            delegate_minimal: false,
        }
    }

    /// Standard profile for IDE + CLI agents (Windsurf, Cursor, Antigravity,
    /// Trae, Codex standalone, Claude Code standalone). Enables all bundles but
    /// intersects with a curated 12-tool allow-list to keep the tool tray small.
    pub(crate) const fn standard() -> Self {
        Self {
            observe: true,
            remember: true,
            coordinate: true,
            operate: true,
            admin: false,
            standard_minimal: true,
            delegate_minimal: false,
        }
    }

    /// Delegate profile for worker agents spawned by tachi_dispatch.
    /// Read + remember bundles only, intersected with a curated 7-tool allow-list.
    /// No dispatch (prevent recursion), no handoff (parent manages), no hub_discover.
    pub(crate) const fn delegate() -> Self {
        Self {
            observe: true,
            remember: true,
            coordinate: false,
            operate: false,
            admin: false,
            standard_minimal: false,
            delegate_minimal: true,
        }
    }

    pub(crate) fn merge(self, other: Self) -> Self {
        Self {
            observe: self.observe || other.observe,
            remember: self.remember || other.remember,
            coordinate: self.coordinate || other.coordinate,
            operate: self.operate || other.operate,
            admin: self.admin || other.admin,
            // Minimal allow-lists are sticky. Standard > delegate if both set.
            standard_minimal: self.standard_minimal || other.standard_minimal,
            delegate_minimal: self.delegate_minimal || other.delegate_minimal,
        }
    }

    pub(crate) fn allows(self, bundle: ToolBundle) -> bool {
        self.admin
            || match bundle {
                ToolBundle::Observe => self.observe,
                ToolBundle::Remember => self.remember,
                ToolBundle::Coordinate => self.coordinate,
                ToolBundle::Operate => self.operate,
            }
    }

    pub(crate) fn as_str(self) -> String {
        if self.admin {
            return "admin".to_string();
        }
        if self.standard_minimal {
            return "standard".to_string();
        }
        if self.delegate_minimal {
            return "delegate".to_string();
        }

        let mut names = Vec::new();
        if self.observe {
            names.push("observe");
        }
        if self.remember {
            names.push("remember");
        }
        if self.coordinate {
            names.push("coordinate");
        }
        if self.operate {
            names.push("operate");
        }
        if names.is_empty() {
            "observe".to_string()
        } else {
            names.join(",")
        }
    }
}

pub(crate) const fn default_tool_profile() -> ToolProfile {
    ToolProfile::standard()
}
