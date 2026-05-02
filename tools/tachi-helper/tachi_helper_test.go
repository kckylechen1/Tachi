package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestScanDotEnvFiles(t *testing.T) {
	// Create a temp .env file
	tmp := t.TempDir()
	envFile := filepath.Join(tmp, ".env")
	os.WriteFile(envFile, []byte("TEST_KEY=hello\nOTHER_VAR=world\n"), 0644)

	// Temporarily change cwd
	old, _ := os.Getwd()
	os.Chdir(tmp)
	defer os.Chdir(old)

	files := scanDotEnvFiles()
	if len(files) == 0 {
		t.Fatal("expected to find at least one .env file")
	}
	found := false
	for _, f := range files {
		if filepath.Base(f) == ".env" {
			found = true
			break
		}
	}
	if !found {
		t.Errorf("did not find .env in results: %v", files)
	}
}

func TestParseDotEnv(t *testing.T) {
	tmp := t.TempDir()
	envFile := filepath.Join(tmp, ".env")
	content := `# comment
KEY1=value1
KEY2="quoted_value"
KEY3='single_quoted'
EMPTY=
# another comment
KEY_WITH_SPACES = spaced value
`
	os.WriteFile(envFile, []byte(content), 0644)

	kvs, err := parseDotEnv(envFile)
	if err != nil {
		t.Fatalf("parseDotEnv error: %v", err)
	}

	if kvs["KEY1"] != "value1" {
		t.Errorf("KEY1 = %q, want %q", kvs["KEY1"], "value1")
	}
	if kvs["KEY2"] != "quoted_value" {
		t.Errorf("KEY2 = %q, want %q", kvs["KEY2"], "quoted_value")
	}
	if kvs["KEY3"] != "single_quoted" {
		t.Errorf("KEY3 = %q, want %q", kvs["KEY3"], "single_quoted")
	}
	if kvs["EMPTY"] != "" {
		t.Errorf("EMPTY = %q, want empty", kvs["EMPTY"])
	}
}

func TestMaskSecret(t *testing.T) {
	tests := []struct {
		input string
		want  string
	}{
		{"short", "****"},
		{"sk-proj-abc123xyz", "sk-p...3xyz"},
		{"abcdefgh", "****"}, // exactly 8 chars: masked
		{"", "****"},
	}
	for _, tt := range tests {
		got := maskSecret(tt.input)
		if got != tt.want {
			t.Errorf("maskSecret(%q) = %q, want %q", tt.input, got, tt.want)
		}
	}
}

func TestDetectShell(t *testing.T) {
	shellType, rcPath := detectShell()
	if shellType == "" {
		t.Error("shellType should not be empty")
	}
	if rcPath == "" {
		t.Error("rcPath should not be empty")
	}
	t.Logf("Detected: %s (%s)", shellType, rcPath)
}

func TestShortPath(t *testing.T) {
	home, _ := os.UserHomeDir()
	if home == "" {
		t.Skip("no home dir")
	}
	path := filepath.Join(home, ".tachi", "memory.db")
	got := shortPath(path)
	if !strings.HasPrefix(got, "~") {
		t.Errorf("shortPath(%q) = %q, want ~ prefix", path, got)
	}
}

func TestMultiSelect(t *testing.T) {
	ms := newMultiSelect([]string{"A", "B", "C"})
	ms.SelectAll()
	selected := ms.Selected()
	if len(selected) != 3 {
		t.Fatalf("SelectAll: got %d items, want 3", len(selected))
	}

	// Deselect first
	delete(ms.selected, 0)
	selected = ms.Selected()
	if len(selected) != 2 {
		t.Fatalf("After deselect: got %d items, want 2", len(selected))
	}
	if selected[0] != "B" || selected[1] != "C" {
		t.Errorf("Selected = %v, want [B C]", selected)
	}
}

func TestFindTachi(t *testing.T) {
	_, err := findTachi()
	// Might not be in PATH on this machine, that's ok
	t.Logf("findTachi: %v", err)
}

func TestMCPClientStart(t *testing.T) {
	_, err := findTachi()
	if err != nil {
		t.Skip("tachi not in PATH, skipping MCP test")
	}

	tc, err := NewMCPClient()
	if err != nil {
		t.Fatalf("NewMCPClient error: %v", err)
	}
	defer tc.Close()
	t.Log("MCP client connected successfully")

	// Test vault_status call
	result, err := tc.CallTool("vault_status", map[string]interface{}{})
	if err != nil {
		t.Fatalf("vault_status call error: %v", err)
	}
	t.Logf("vault_status result: %s", string(result))
}

func TestWizardInit(t *testing.T) {
	w := newWizard()
	if w.current != stepWelcome {
		t.Errorf("initial step = %d, want %d", w.current, stepWelcome)
	}
	if len(w.steps) != int(stepCount) {
		t.Errorf("steps count = %d, want %d", len(w.steps), stepCount)
	}
}

func TestAppendToRC(t *testing.T) {
	tmp := t.TempDir()
	rcFile := filepath.Join(tmp, ".zshrc")

	// Write initial content
	os.WriteFile(rcFile, []byte("export PATH=$HOME/bin:$PATH\n"), 0644)

	err := appendToRC(rcFile)
	if err != nil {
		t.Fatalf("appendToRC error: %v", err)
	}

	data, _ := os.ReadFile(rcFile)
	content := string(data)
	if !strings.Contains(content, "tachi_env()") {
		t.Errorf("rc file doesn't contain helper function: %s", content)
	}

	// Should be idempotent
	err = appendToRC(rcFile)
	if err != nil {
		t.Fatalf("second appendToRC error: %v", err)
	}
	data, _ = os.ReadFile(rcFile)
	lines := strings.Count(string(data), "tachi_env()")
	if lines != 1 {
		t.Errorf("helper function appears %d times, want 1", lines)
	}
}
