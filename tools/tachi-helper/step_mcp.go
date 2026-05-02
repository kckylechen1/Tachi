package main

import (
	"fmt"
	"strings"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

type mcpPhase int

const (
	mcpPhaseSelect mcpPhase = iota
	mcpPhaseRegistering
	mcpPhaseDone
	mcpPhaseSkip
)

type mcpStep struct {
	state     *State
	phase     mcpPhase
	ms        *multiSelect
	errMsg    string
	registered int
}

func newMCPStep(state *State) *mcpStep {
	return &mcpStep{state: state}
}

func (s *mcpStep) title() string { return "MCP Servers" }
func (s *mcpStep) subtitle() string {
	return "Register MCP servers in the Tachi Hub for all agents to use"
}

func (s *mcpStep) Init() tea.Cmd {
	items := make([]string, len(mcpServerDefs))
	for i, mcp := range mcpServerDefs {
		label := mcp.Name
		if mcp.NeedsKey != "" {
			label += fmt.Sprintf(" (requires %s)", mcp.NeedsKey)
		}
		items[i] = label
	}
	s.ms = newMultiSelect(items)
	s.phase = mcpPhaseSelect
	return nil
}

func (s *mcpStep) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case mcpRegisteredMsg:
		s.phase = mcpPhaseDone
		s.registered = msg.count
		return s, nil
	case mcpErrorMsg:
		s.phase = mcpPhaseDone
		s.errMsg = msg.err
		return s, nil
	case tea.KeyMsg:
		switch msg.String() {
		case "enter":
			if s.phase == mcpPhaseSelect {
				if len(s.ms.Selected()) == 0 {
					s.phase = mcpPhaseSkip
					return s, nil
				}
				s.state.SelectedMCPs = s.ms.Selected()
				s.phase = mcpPhaseRegistering
				return s, s.registerMCPs()
			}
			if s.phase == mcpPhaseDone || s.phase == mcpPhaseSkip {
				return s, stepDone()
			}
		case "s":
			if s.phase == mcpPhaseSelect {
				s.phase = mcpPhaseSkip
				return s, nil
			}
		}
	}

	if s.phase == mcpPhaseSelect {
		if km, ok := msg.(tea.KeyMsg); ok {
			s.ms.Update(km)
		}
	}
	return s, nil
}

func (s *mcpStep) View() string {
	var sb strings.Builder
	sb.WriteString("\n")

	switch s.phase {
	case mcpPhaseSelect:
		sb.WriteString("  Select MCP servers to register:\n\n")
		sb.WriteString(s.ms.View())
		sb.WriteString(hintStyle.Render("\n  enter: register selected  ·  s: skip"))

	case mcpPhaseRegistering:
		sb.WriteString("  " + lipgloss.NewStyle().Foreground(accent).Render("⠋") +
			fmt.Sprintf(" Registering %d MCP server(s)...\n", len(s.state.SelectedMCPs)))

	case mcpPhaseSkip:
		sb.WriteString(warnStyle.Render("  ⊘ Skipped") + " — no MCP servers registered\n")
		sb.WriteString("\n" + hintStyle.Render("  Press Enter to continue"))

	case mcpPhaseDone:
		if s.errMsg != "" {
			sb.WriteString("  " + crossStyle.Render("✗ Registration failed: "+s.errMsg) + "\n")
		} else {
			sb.WriteString("  " + checkStyle.Render(fmt.Sprintf("✓ Registered %d MCP server(s)", s.registered)) + "\n")
		}
		sb.WriteString("\n" + hintStyle.Render("  Press Enter to continue"))
	}

	return sb.String()
}

type mcpRegisteredMsg struct{ count int }
type mcpErrorMsg struct{ err string }

func (s *mcpStep) registerMCPs() tea.Cmd {
	return func() tea.Msg {
		count := 0
		for _, label := range s.state.SelectedMCPs {
			// Find matching server def
			var def *struct {
				ID       string
				Name     string
				DefJSON  string
				NeedsKey string
			}
			for i, d := range mcpServerDefs {
				itemLabel := d.Name
				if d.NeedsKey != "" {
					itemLabel += fmt.Sprintf(" (requires %s)", d.NeedsKey)
				}
				if itemLabel == label {
					def = &mcpServerDefs[i]
					break
				}
			}
			if def == nil {
				continue
			}

			// Register via tachi hub CLI
			err := registerHubMCP(def.ID, def.Name, def.DefJSON)
			if err != nil {
				return mcpErrorMsg{err: fmt.Sprintf("%s: %s", def.Name, err.Error())}
			}
			count++
		}
		return mcpRegisteredMsg{count: count}
	}
}

func registerHubMCP(id, name, definition string) error {
	// Use tachi hub register CLI command
	// tachi hub register --id mcp:exa --cap-type mcp --name "Exa" --definition '{...}'
	return nil // TODO: exec tachi hub register
}
