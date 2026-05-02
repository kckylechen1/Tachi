package main

import (
	"fmt"
	"strings"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

type welcomeStep struct{}

func newWelcomeStep() *welcomeStep {
	return &welcomeStep{}
}

func (s *welcomeStep) title() string    { return "Welcome" }
func (s *welcomeStep) subtitle() string { return "" }

func (s *welcomeStep) Init() tea.Cmd { return nil }

func (s *welcomeStep) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.KeyMsg:
		switch msg.String() {
		case "enter", " ":
			return s, stepDone()
		}
	}
	return s, nil
}

func (s *welcomeStep) View() string {
	logo := lipgloss.NewStyle().Foreground(accent).Bold(true).Render(
		strings.Join([]string{
			"",
			"  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━",
			"    Tachi Setup Wizard",
			"  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━",
			"",
		}, "\n"),
	)

	steps := []string{
		"Check your Tachi installation",
		"Initialize the encrypted Vault",
		"Import API keys from .env files",
		"Register MCP servers",
		"Set up shell integration",
	}

	var stepList strings.Builder
	stepList.WriteString("\n")
	for i, s := range steps {
		num := lipgloss.NewStyle().Foreground(accent).Bold(true).Render(fmt.Sprintf("%d.", i+1))
		stepList.WriteString(fmt.Sprintf("  %s  %s\n", num, s))
	}

	desc := lipgloss.NewStyle().Foreground(dimText).Render(
		"\n  Your API keys will be stored encrypted and never\n  committed to git.",
	)

	enter := lipgloss.NewStyle().Foreground(accent).Bold(true).Render(
		fmt.Sprintf("\n  Press %s to start", "Enter"),
	)

	return logo + stepList.String() + desc + enter
}
