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
	TachiMissing bool

	// Keys
	DotEnvFiles  []string
	FoundKeys    []EnvKey
	SelectedKeys []string

	// Providers
	ProviderSelections map[string]ProviderSelection

	// MCP
	RegisteredMCPs []string
	SelectedMCPs   []string

	// Foundry
	FoundrySelections map[string]string

	// Shell
	ShellType  string
	ShellRC    string
	RCModified bool
}

type EnvKey struct {
	File   string
	Name   string
	Value  string
	Masked string
}

type ProviderSelection struct {
	CategoryID string
	Name       string
	ProviderID string
	APIKeyEnv  string
	Endpoint   string
	Model      string
}
