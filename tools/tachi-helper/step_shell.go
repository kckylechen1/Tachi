package main

import (
	"fmt"
	"strings"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

type shellPhase int

const (
	shellPhaseConfirm shellPhase = iota
	shellPhaseWriting
	shellPhaseDone
	shellPhaseSkip
)

type shellStep struct {
	state   *State
	phase   shellPhase
	errMsg  string
	written bool
}

func newShellStep(state *State) *shellStep {
	return &shellStep{state: state}
}

func (s *shellStep) title() string    { return "Shell Integration" }
func (s *shellStep) subtitle() string {
	return fmt.Sprintf("Add eval \"$(tachi env)\" to your %s", s.state.ShellRC)
}

func (s *shellStep) Init() tea.Cmd {
	s.phase = shellPhaseConfirm
	return nil
}

func (s *shellStep) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case shellWrittenMsg:
		s.phase = shellPhaseDone
		s.written = true
		s.state.RCModified = true
		return s, nil
	case shellErrorMsg:
		s.phase = shellPhaseDone
		s.errMsg = msg.err
		return s, nil
	case tea.KeyMsg:
		switch msg.String() {
		case "y", "Y", "enter":
			if s.phase == shellPhaseConfirm {
				s.phase = shellPhaseWriting
				return s, s.writeRC()
			}
			if s.phase == shellPhaseDone || s.phase == shellPhaseSkip {
				return s, stepDone()
			}
		case "n", "N":
			if s.phase == shellPhaseConfirm {
				s.phase = shellPhaseSkip
				return s, nil
			}
		}
	}
	return s, nil
}

func (s *shellStep) View() string {
	var sb strings.Builder
	sb.WriteString("\n")

	switch s.phase {
	case shellPhaseConfirm:
		evalLine := `eval "$(tachi env)"`
		sb.WriteString("  The following line will be appended to ")
		sb.WriteString(lipgloss.NewStyle().Foreground(accent).Bold(true).Render(shortPath(s.state.ShellRC)))
		sb.WriteString(":\n\n")
		sb.WriteString("  " + codeBlockStyle.Render(evalLine))
		sb.WriteString("\n\n")
		sb.WriteString("  This loads your Vault secrets into the shell environment\n")
		sb.WriteString("  on every terminal start, replacing .env files.\n\n")
		sb.WriteString(hintStyle.Render("  enter/y: add to " + s.state.ShellType + "rc  ·  n: skip"))

	case shellPhaseWriting:
		sb.WriteString("  " + lipgloss.NewStyle().Foreground(accent).Render("⠋") + " Writing to " + shortPath(s.state.ShellRC) + "...\n")

	case shellPhaseSkip:
		sb.WriteString(warnStyle.Render("  ⊘ Skipped") + " — shell rc not modified\n")
		sb.WriteString("\n  You can add it manually later:\n")
		sb.WriteString("  " + codeBlockStyle.Render(`eval "$(tachi env)"`))
		sb.WriteString("\n\n" + hintStyle.Render("  Press Enter to continue"))

	case shellPhaseDone:
		if s.errMsg != "" {
			sb.WriteString("  " + crossStyle.Render("✗ "+s.errMsg) + "\n")
			sb.WriteString("\n  Add manually:\n")
			sb.WriteString("  " + codeBlockStyle.Render(`eval "$(tachi env)"`))
		} else {
			sb.WriteString("  " + checkStyle.Render("✓ Shell integration added to "+shortPath(s.state.ShellRC)) + "\n")
			sb.WriteString("\n  Run to activate now:\n")
			sb.WriteString("  " + codeBlockStyle.Render(`source `+s.state.ShellRC))
		}
		sb.WriteString("\n\n" + hintStyle.Render("  Press Enter to continue"))
	}

	return sb.String()
}

type shellWrittenMsg struct{}
type shellErrorMsg struct{ err string }

func (s *shellStep) writeRC() tea.Cmd {
	return func() tea.Msg {
		if err := appendToRC(s.state.ShellRC, `eval "$(tachi env)"`); err != nil {
			return shellErrorMsg{err: err.Error()}
		}
		return shellWrittenMsg{}
	}
}
