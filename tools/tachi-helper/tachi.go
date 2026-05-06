package main

import (
	"bufio"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
)

// findTachi locates the tachi binary in PATH.
func findTachi() (string, error) {
	p, err := exec.LookPath("tachi")
	if err != nil {
		return "", fmt.Errorf("tachi binary not found in PATH")
	}
	return p, nil
}

// tachiVersion runs `tachi --version` and returns the version string.
func tachiVersion() string {
	out, err := exec.Command("tachi", "--version").Output()
	if err != nil {
		return "unknown"
	}
	return strings.TrimSpace(string(out))
}

// parseDotEnv reads a .env file and returns key=value pairs.
func parseDotEnv(path string) (map[string]string, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()

	result := make(map[string]string)
	scanner := bufio.NewScanner(f)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		idx := strings.Index(line, "=")
		if idx < 1 {
			continue
		}
		key := strings.TrimSpace(line[:idx])
		val := strings.TrimSpace(line[idx+1:])
		// Strip surrounding quotes
		if (strings.HasPrefix(val, `"`) && strings.HasSuffix(val, `"`)) ||
			(strings.HasPrefix(val, "'") && strings.HasSuffix(val, "'")) {
			val = val[1 : len(val)-1]
		}
		if key != "" {
			result[key] = val
		}
	}
	return result, scanner.Err()
}

// scanDotEnvFiles searches for .env files in common locations.
func scanDotEnvFiles() []string {
	var files []string
	seen := make(map[string]bool)

	candidates := []string{
		".env",
		".tachi/config.env",
	}

	if home, err := os.UserHomeDir(); err == nil {
		candidates = append(candidates,
			filepath.Join(home, ".env"),
			filepath.Join(home, ".tachi", "config.env"),
			filepath.Join(home, ".secrets", "master.env"),
		)
	}

	if cwd, err := os.Getwd(); err == nil {
		// Walk up to 3 levels looking for .env
		dir := cwd
		for i := 0; i < 4; i++ {
			p := filepath.Join(dir, ".env")
			if abs, err := filepath.Abs(p); err == nil {
				candidates = append(candidates, abs)
			}
			parent := filepath.Dir(dir)
			if parent == dir {
				break
			}
			dir = parent
		}
	}

	for _, c := range candidates {
		abs, err := filepath.Abs(c)
		if err != nil {
			abs = c
		}
		if seen[abs] {
			continue
		}
		seen[abs] = true
		if info, err := os.Stat(abs); err == nil && !info.IsDir() {
			files = append(files, abs)
		}
	}
	return files
}

// maskSecret masks a secret value for display.
func maskSecret(val string) string {
	if len(val) <= 8 {
		return "****"
	}
	return val[:4] + "..." + val[len(val)-4:]
}

// detectShell returns the user's shell type and rc file path.
func detectShell() (shellType, rcPath string) {
	shell := os.Getenv("SHELL")
	switch {
	case strings.Contains(shell, "zsh"):
		home, _ := os.UserHomeDir()
		return "zsh", filepath.Join(home, ".zshrc")
	case strings.Contains(shell, "bash"):
		home, _ := os.UserHomeDir()
		return "bash", filepath.Join(home, ".bashrc")
	case strings.Contains(shell, "fish"):
		home, _ := os.UserHomeDir()
		return "fish", filepath.Join(home, ".config", "fish", "config.fish")
	default:
		home, _ := os.UserHomeDir()
		return "unknown", filepath.Join(home, ".profile")
	}
}

// appendToRC appends the tachi_env helper function to the shell rc file if not already present.
func appendToRC(rcPath string) error {
	// Read existing content
	data, err := os.ReadFile(rcPath)
	if err != nil && !os.IsNotExist(err) {
		return err
	}

	content := string(data)
	if strings.Contains(content, "tachi_env()") {
		return nil // already present
	}

	f, err := os.OpenFile(rcPath, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0644)
	if err != nil {
		return err
	}
	defer f.Close()

	block := "\n# Tachi — run `tachi_env` to load vault secrets into shell\n" + tachiEnvHelperBlock() + "\n"
	_, err = f.WriteString(block)
	return err
}

func tachiEnvHelperBlock() string {
	if runtime.GOOS == "darwin" {
		return `tachi_env() {
  if [ -n "${TACHI_VAULT_PASSWORD_FILE:-}" ]; then
    eval "$(tachi env --password-file "$TACHI_VAULT_PASSWORD_FILE" --env-only)"
  else
    eval "$(tachi env --keychain --env-only)"
  fi
}`
	}
	return `tachi_env() {
  if [ -n "${TACHI_VAULT_PASSWORD_FILE:-}" ]; then
    eval "$(tachi env --password-file "$TACHI_VAULT_PASSWORD_FILE" --env-only)"
  else
    eval "$(tachi env --env-only)"
  fi
}`
}

// mcpServerDefs contains common MCP servers users might want to register.
var mcpServerDefs = []struct {
	ID       string
	Name     string
	DefJSON  string
	NeedsKey string
}{
	{
		ID:       "mcp:exa",
		Name:     "Exa (Web Search)",
		DefJSON:  `{"transport":"stdio","command":"npx","args":["-y","exa-mcp-server"],"discovered_tools":[]}`,
		NeedsKey: "EXA_API_KEY",
	},
	{
		ID:       "mcp:tavily",
		Name:     "Tavily (Web Search)",
		DefJSON:  `{"transport":"stdio","command":"npx","args":["-y","tavily-mcp"],"discovered_tools":[]}`,
		NeedsKey: "TAVILY_API_KEY",
	},
	{
		ID:      "mcp:context7",
		Name:    "Context7 (Documentation)",
		DefJSON: `{"transport":"stdio","command":"npx","args":["-y","@upstash/context7-mcp@latest"],"discovered_tools":[]}`,
	},
	{
		ID:      "mcp:memory",
		Name:    "Memory (Persistent Memory)",
		DefJSON: `{"transport":"stdio","command":"npx","args":["-y","@modelcontextprotocol/server-memory"],"discovered_tools":[]}`,
	},
	{
		ID:      "mcp:filesystem",
		Name:    "Filesystem (File Access)",
		DefJSON: `{"transport":"stdio","command":"npx","args":["-y","@modelcontextprotocol/server-filesystem","/"],"discovered_tools":[]}`,
	},
}

type vaultStatus struct {
	Initialized bool `json:"initialized"`
	Locked      bool `json:"locked"`
	EntryCount  int  `json:"entry_count"`
}

func getVaultStatus() (vaultStatus, error) {
	tc, err := NewMCPClient()
	if err != nil {
		return vaultStatus{}, err
	}
	defer tc.Close()

	result, err := tc.CallTool("vault_status", map[string]interface{}{})
	if err != nil {
		return vaultStatus{}, err
	}

	text, err := toolResultText(result)
	if err != nil {
		return vaultStatus{}, err
	}

	var status vaultStatus
	if err := json.Unmarshal([]byte(text), &status); err != nil {
		return vaultStatus{}, fmt.Errorf("parse vault status: %w", err)
	}
	return status, nil
}

func toolResultText(data []byte) (string, error) {
	var result struct {
		Content []struct {
			Type string `json:"type"`
			Text string `json:"text"`
		} `json:"content"`
		IsError bool `json:"isError"`
	}
	if err := json.Unmarshal(data, &result); err != nil {
		return "", fmt.Errorf("parse tool result: %w", err)
	}
	if result.IsError {
		return "", fmt.Errorf("tool returned error")
	}
	for _, item := range result.Content {
		if item.Type == "text" && item.Text != "" {
			return item.Text, nil
		}
	}
	return "", fmt.Errorf("tool result contained no text")
}

func generatePassword() string {
	b := make([]byte, 32)
	rand.Read(b)
	return hex.EncodeToString(b)
}

func storeInKeychain(password string) error {
	cmd := exec.Command("security", "add-generic-password",
		"-U",
		"-s", "tachi-vault",
		"-a", "default",
		"-w", password,
	)
	out, err := cmd.CombinedOutput()
	if err != nil {
		return fmt.Errorf("%s: %s", err, strings.TrimSpace(string(out)))
	}
	return nil
}

func readFromKeychain() (string, error) {
	out, err := exec.Command("security", "find-generic-password",
		"-s", "tachi-vault",
		"-a", "default",
		"-w",
	).Output()
	if err != nil {
		return "", fmt.Errorf("failed to read from Keychain: %s", strings.TrimSpace(string(out)))
	}
	return strings.TrimSpace(string(out)), nil
}
