package main

// State is shared across all wizard steps.
type State struct {
	// Doctor
	TachiPath    string
	TachiVersion string
	GlobalDBPath string
	VaultInit    bool
	VaultLocked  bool
	VaultEntries int

	// Vault
	Password string

	// Keys
	DotEnvFiles []string
	FoundKeys   []EnvKey
	SelectedKeys []string

	// MCP
	RegisteredMCPs []string
	SelectedMCPs   []string

	// Shell
	ShellType string
	ShellRC   string
	RCModified bool
}

type EnvKey struct {
	File    string
	Name    string
	Value   string
	Masked  string
}
