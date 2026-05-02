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
			"  ╺━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸",
			"      Tachi Setup Wizard",
			"  ╺━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸",
			"",
		}, "\n"),
	)

	desc := lipgloss.NewStyle().Foreground(normal).Render(
		"  This wizard will guide you through setting up Tachi:\n" +
			"\n" +
			"  1.  Check your Tachi installation\n" +
			"  2.  Initialize the encrypted Vault\n" +
			"  3.  Import API keys from .env files\n" +
			"  4.  Register MCP servers\n" +
			"  5.  Set up shell integration\n" +
			"\n" +
			"  Your API keys will be stored encrypted and never\n" +
			"  committed to git.",
	)

	enter := lipgloss.NewStyle().Foreground(accent).Bold(true).Render(
		fmt.Sprintf("\n  Press %s to start", "Enter"),
	)

	return logo + desc + enter
}
