package main

import (
	"strings"

	"github.com/charmbracelet/bubbles/textinput"
	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

type vaultPhase int

const (
	vaultPhasePassword vaultPhase = iota
	vaultPhaseConfirm
	vaultPhaseCreating
	vaultPhaseDone
	vaultPhaseError
)

type vaultStep struct {
	state       *State
	phase       vaultPhase
	password    textinput.Model
	confirm     textinput.Model
	errMsg      string
	init        bool // true = init, false = just unlock
}

func newVaultStep(state *State) *vaultStep {
	ti := textinput.New()
	ti.EchoMode = textinput.EchoPassword
	ti.EchoCharacter = '•'
	ti.Placeholder = "Master password"
	ti.Focus()

	ci := textinput.New()
	ci.EchoMode = textinput.EchoPassword
	ci.EchoCharacter = '•'
	ci.Placeholder = "Confirm password"

	return &vaultStep{
		state:    state,
		phase:    vaultPhasePassword,
		password: ti,
		confirm:  ci,
		init:     true, // first time setup
	}
}

func (s *vaultStep) title() string { return "Vault Setup" }
func (s *vaultStep) subtitle() string {
	if s.init {
		return "Create a master password for the encrypted Vault"
	}
	return "Enter your Vault password to unlock"
}

func (s *vaultStep) Init() tea.Cmd { return textinput.Blink }

func (s *vaultStep) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case vaultCreatedMsg:
		s.phase = vaultPhaseDone
		return s, nil
	case vaultErrorMsg:
		s.phase = vaultPhaseError
		s.errMsg = msg.err
		return s, nil
	case tea.KeyMsg:
		switch msg.String() {
		case "enter":
			return s.handleEnter()
		case "ctrl+c":
			return s, tea.Quit
		}
	}

	var cmd tea.Cmd
	if s.phase == vaultPhasePassword {
		s.password, cmd = s.password.Update(msg)
	} else if s.phase == vaultPhaseConfirm {
		s.confirm, cmd = s.confirm.Update(msg)
	}
	return s, cmd
}

func (s *vaultStep) handleEnter() (tea.Model, tea.Cmd) {
	switch s.phase {
	case vaultPhasePassword:
		pwd := s.password.Value()
		if len(pwd) < 8 {
			s.errMsg = "Password must be at least 8 characters"
			return s, nil
		}
		s.errMsg = ""
		if s.init {
			s.phase = vaultPhaseConfirm
			s.confirm.Focus()
			return s, textinput.Blink
		}
		// unlock existing vault
		s.state.Password = pwd
		return s, vaultUnlockCmd(pwd)

	case vaultPhaseConfirm:
		if s.confirm.Value() != s.password.Value() {
			s.errMsg = "Passwords don't match"
			s.confirm.SetValue("")
			return s, nil
		}
		s.state.Password = s.password.Value()
		s.phase = vaultPhaseCreating
		return s, vaultInitCmd(s.password.Value())

	case vaultPhaseError:
		// Reset and retry
		s.phase = vaultPhasePassword
		s.password.SetValue("")
		s.confirm.SetValue("")
		s.errMsg = ""
		s.password.Focus()
		return s, textinput.Blink
	}
	return s, nil
}

func (s *vaultStep) View() string {
	var sb strings.Builder
	sb.WriteString("\n")

	switch s.phase {
	case vaultPhasePassword, vaultPhaseError:
		sb.WriteString("  Enter master password:\n\n")
		sb.WriteString("  " + s.password.View() + "\n")
		if s.errMsg != "" {
			sb.WriteString("\n  " + crossStyle.Render("✗ "+s.errMsg) + "\n")
		}

	case vaultPhaseConfirm:
		sb.WriteString("  Confirm master password:\n\n")
		sb.WriteString("  " + s.confirm.View() + "\n")
		if s.errMsg != "" {
			sb.WriteString("\n  " + crossStyle.Render("✗ "+s.errMsg) + "\n")
		}

	case vaultPhaseCreating:
		sb.WriteString("  " + lipgloss.NewStyle().Foreground(accent).Render("⠋") + " Initializing vault...\n")

	case vaultPhaseDone:
		sb.WriteString("  " + checkStyle.Render("✓ Vault initialized successfully") + "\n")
		sb.WriteString("\n" + hintStyle.Render("  Press Enter to continue"))
	}

	return sb.String()
}

// --- Commands ---

type vaultCreatedMsg struct{}
type vaultErrorMsg struct {
	err string
}

func vaultInitCmd(password string) tea.Cmd {
	return func() tea.Msg {
		tc, err := NewMCPClient()
		if err != nil {
			return vaultErrorMsg{err: "Failed to start tachi: " + err.Error()}
		}
		defer tc.Close()

		_, err = tc.CallTool("vault_init", map[string]interface{}{
			"password": password,
		})
		if err != nil {
			return vaultErrorMsg{err: err.Error()}
		}
		return vaultCreatedMsg{}
	}
}

func vaultUnlockCmd(password string) tea.Cmd {
	return func() tea.Msg {
		tc, err := NewMCPClient()
		if err != nil {
			return vaultErrorMsg{err: "Failed to start tachi: " + err.Error()}
		}
		defer tc.Close()

		_, err = tc.CallTool("vault_unlock", map[string]interface{}{
			"password": password,
		})
		if err != nil {
			return vaultErrorMsg{err: err.Error()}
		}
		return vaultCreatedMsg{}
	}
}
