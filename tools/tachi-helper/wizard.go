package main

import (
	"fmt"
	"strings"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

type stepID int

const (
	stepWelcome stepID = iota
	stepDoctor
	stepVault
	stepKeys
	stepMCP
	stepShell
	stepSummary
	stepCount
)

type stepDoneMsg struct{}
type stepBackMsg struct{}

type wizard struct {
	current stepID
	steps   []step
	state   *State
	mcp     *MCPClient
	width   int
	height  int
	quitting bool
}

type step interface {
	tea.Model
	title() string
	subtitle() string
}

func stepDone() tea.Cmd {
	return func() tea.Msg { return stepDoneMsg{} }
}

func stepBack() tea.Cmd {
	return func() tea.Msg { return stepBackMsg{} }
}

func newWizard() *wizard {
	state := &State{}
	w := &wizard{
		state: state,
		steps: make([]step, stepCount),
	}
	w.steps[stepWelcome] = newWelcomeStep()
	w.steps[stepDoctor] = newDoctorStep(state)
	w.steps[stepVault] = newVaultStep(state)
	w.steps[stepKeys] = newKeysStep(state)
	w.steps[stepMCP] = newMCPStep(state)
	w.steps[stepShell] = newShellStep(state)
	w.steps[stepSummary] = newDoneStep(state)
	return w
}

func (w *wizard) Init() tea.Cmd {
	return w.steps[w.current].Init()
}

func (w *wizard) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		w.width = msg.Width
		w.height = msg.Height
		return w, nil

	case tea.KeyMsg:
		switch msg.String() {
		case "ctrl+c", "q":
			if w.current == stepSummary {
				w.quitting = true
				return w, tea.Quit
			}
		case "esc":
			if w.current == stepSummary {
				w.quitting = true
				return w, tea.Quit
			}
		}

	case stepDoneMsg:
		if w.current < stepCount-1 {
			w.current++
			return w, w.steps[w.current].Init()
		}
		w.quitting = true
		return w, tea.Quit

	case stepBackMsg:
		if w.current > stepWelcome {
			w.current--
			return w, w.steps[w.current].Init()
		}
	}

	m, cmd := w.steps[w.current].Update(msg)
	w.steps[w.current] = m.(step)
	return w, cmd
}

func (w *wizard) View() string {
	if w.quitting {
		return ""
	}

	s := w.steps[w.current]

	var sb strings.Builder
	sb.WriteString(w.renderHeader(s))
	sb.WriteString("\n\n")
	sb.WriteString(s.View())
	sb.WriteString("\n")
	sb.WriteString(w.renderFooter())

	return boxStyle.
		Width(max(w.width-4, 50)).
		Height(max(w.height-2, 20)).
		Render(sb.String())
}

func (w *wizard) renderHeader(s step) string {
	title := headerBarStyle.Render(fmt.Sprintf("Tachi Setup  ·  Step %d of %d", w.current+1, stepCount))

	var steps []string
	labels := []string{"Welcome", "Health Check", "Vault", "Keys", "MCP", "Shell", "Done"}
	for i, label := range labels {
		switch {
		case stepID(i) < w.current:
			steps = append(steps, stepDoneStyle.Render("✓ "+label))
		case stepID(i) == w.current:
			steps = append(steps, stepActiveStyle.Render("● "+label))
		default:
			steps = append(steps, stepPendingStyle.Render("○ "+label))
		}
	}
	progress := lipgloss.JoinHorizontal(lipgloss.Top, strings.Join(steps, stepPendingStyle.Render("  ")))

	sub := subtitleStyle.Render(s.subtitle())
	if sub != "" {
		sub = "\n" + sub
	}

	return title + "\n" + progress + sub
}

func (w *wizard) renderFooter() string {
	if w.current == stepWelcome {
		return hintStyle.Render("enter: start  ·  q: quit")
	}
	if w.current == stepSummary {
		return hintStyle.Render("q / esc: quit")
	}
	return hintStyle.Render("enter: next  ·  esc: back  ·  q: quit")
}
