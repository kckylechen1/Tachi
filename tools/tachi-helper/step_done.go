package main

import (
	"fmt"
	"strings"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

type doneStep struct {
	state *State
}

func newDoneStep(state *State) *doneStep {
	return &doneStep{state: state}
}

func (s *doneStep) title() string    { return "Setup Complete" }
func (s *doneStep) subtitle() string { return "Summary of what was configured" }

func (s *doneStep) Init() tea.Cmd { return nil }

func (s *doneStep) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	return s, nil
}

func (s *doneStep) View() string {
	var sb strings.Builder
	sb.WriteString("\n")

	sb.WriteString(checkStyle.Render("  ✓ Tachi setup complete!") + "\n\n")
	sb.WriteString("  ─────────────────────────────────────\n\n")

	items := []struct {
		label string
		value string
	}{
		{"Tachi", s.state.TachiVersion},
		{"Vault", fmt.Sprintf("initialized (%d keys)", len(s.state.SelectedKeys))},
		{"Keys imported", fmt.Sprintf("%d", len(s.state.SelectedKeys))},
		{"MCP servers", fmt.Sprintf("%d registered", len(s.state.SelectedMCPs))},
		{"Shell", fmt.Sprintf("%s (%s)", s.state.ShellType, boolMark(s.state.RCModified))},
	}

	for _, item := range items {
		sb.WriteString(fmt.Sprintf("  %-16s %s\n",
			lipgloss.NewStyle().Foreground(dimText).Render(item.label+":"),
			lipgloss.NewStyle().Foreground(normal).Render(item.value),
		))
	}

	sb.WriteString("\n  ─────────────────────────────────────\n\n")
	sb.WriteString("  Next steps:\n\n")
	sb.WriteString("  1. " + lipgloss.NewStyle().Foreground(bright).Render(`Run: eval "$(tachi env)"`) + "\n")
	sb.WriteString("     to load secrets into your current shell\n\n")
	sb.WriteString("  2. Store more keys with your MCP client:\n")
	sb.WriteString("     " + lipgloss.NewStyle().Foreground(dimText).Render(`tachi vault_set(name="KEY_NAME", value="secret")`) + "\n\n")
	sb.WriteString("  3. Remove .env files from your projects:\n")
	sb.WriteString("     " + lipgloss.NewStyle().Foreground(dimText).Render(`git rm .env && echo ".env" >> .gitignore`) + "\n")

	sb.WriteString("\n\n" + hintStyle.Render("  q / esc: quit"))

	return sb.String()
}

func boolMark(b bool) string {
	if b {
		return checkStyle.Render("rc modified")
	}
	return "skipped"
}
