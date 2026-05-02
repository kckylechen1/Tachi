package main

import (
	"fmt"
	"os"
	"os/user"
	"path/filepath"
	"strings"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

type doctorStep struct {
	state  *State
	done   bool
	checks []checkResult
}

type checkResult struct {
	label  string
	ok     bool
	detail string
}

func newDoctorStep(state *State) *doctorStep {
	return &doctorStep{state: state}
}

func (s *doctorStep) title() string    { return "Health Check" }
func (s *doctorStep) subtitle() string { return "Checking Tachi installation and configuration" }

func (s *doctorStep) Init() tea.Cmd {
	return runDoctorChecks(s.state)
}

func (s *doctorStep) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case doctorResultsMsg:
		s.checks = msg.results
		s.done = true
		return s, nil
	case tea.KeyMsg:
		if s.done && msg.String() == "enter" {
			return s, stepDone()
		}
	}
	return s, nil
}

func (s *doctorStep) View() string {
	if len(s.checks) == 0 {
		return fmt.Sprintf("\n  %s Running checks...\n",
			lipgloss.NewStyle().Foreground(accent).Render("⠋"))
	}

	var sb strings.Builder
	sb.WriteString("\n")
	for _, c := range s.checks {
		icon := checkStyle.Render("✓")
		if !c.ok {
			icon = crossStyle.Render("✗")
		}
		line := fmt.Sprintf("  %s %s", icon, c.label)
		if c.detail != "" {
			line += lipgloss.NewStyle().Foreground(dimText).Render("  " + c.detail)
		}
		sb.WriteString(line + "\n")
	}

	if s.done {
		sb.WriteString("\n" + hintStyle.Render("  Press Enter to continue"))
	}
	return sb.String()
}

type doctorResultsMsg struct {
	results []checkResult
}

func runDoctorChecks(state *State) tea.Cmd {
	return func() tea.Msg {
		var results []checkResult

		// Check 1: tachi binary
		path, err := findTachi()
		if err != nil {
			results = append(results, checkResult{
				label:  "tachi binary",
				ok:     false,
				detail: "not found in PATH — install tachi first",
			})
		} else {
			ver := tachiVersion()
			results = append(results, checkResult{
				label:  "tachi binary",
				ok:     true,
				detail: fmt.Sprintf("%s (%s)", path, ver),
			})
			state.TachiPath = path
			state.TachiVersion = ver
		}

		// Check 2: global DB
		home, _ := os.UserHomeDir()
		dbPath := filepath.Join(home, ".tachi", "global", "memory.db")
		if _, err := os.Stat(dbPath); err == nil {
			state.GlobalDBPath = dbPath
			results = append(results, checkResult{
				label:  "Global database",
				ok:     true,
				detail: shortPath(dbPath),
			})
		} else {
			results = append(results, checkResult{
				label:  "Global database",
				ok:     false,
				detail: "not found — will be created on first run",
			})
		}

		// Check 3: vault
		state.VaultInit = false
		state.VaultLocked = true
		results = append(results, checkResult{
			label:  "Vault",
			ok:     false,
			detail: "not initialized — will set up next",
		})

		// Check 4: .env files
		envFiles := scanDotEnvFiles()
		state.DotEnvFiles = envFiles
		if len(envFiles) > 0 {
			results = append(results, checkResult{
				label:  fmt.Sprintf(".env files (%d found)", len(envFiles)),
				ok:     false,
				detail: strings.Join(mapStr(envFiles, shortPath), ", "),
			})
		} else {
			results = append(results, checkResult{
				label:  ".env files",
				ok:     true,
				detail: "none found — clean setup",
			})
		}

		// Check 5: shell
		shellType, rcPath := detectShell()
		state.ShellType = shellType
		state.ShellRC = rcPath
		results = append(results, checkResult{
			label:  fmt.Sprintf("Shell (%s)", shellType),
			ok:     true,
			detail: shortPath(rcPath),
		})

		return doctorResultsMsg{results: results}
	}
}

func shortPath(path string) string {
	home, _ := os.UserHomeDir()
	if home != "" && strings.HasPrefix(path, home) {
		return "~" + path[len(home):]
	}
	return path
}

func mapStr(slice []string, fn func(string) string) []string {
	out := make([]string, len(slice))
	for i, s := range slice {
		out[i] = fn(s)
	}
	return out
}

func userHomeDir() (string, error) {
	u, err := user.Current()
	if err != nil {
		return os.UserHomeDir()
	}
	return u.HomeDir, nil
}
