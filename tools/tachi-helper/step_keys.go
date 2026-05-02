package main

import (
	"fmt"
	"strings"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

type keysPhase int

const (
	keysPhaseScan keysPhase = iota
	keysPhaseSelect
	keysPhaseImporting
	keysPhaseDone
	keysPhaseSkip
)

type keysStep struct {
	state  *State
	phase  keysPhase
	ms     *multiSelect
	errMsg string
	imported int
	skipped  int
}

func newKeysStep(state *State) *keysStep {
	return &keysStep{state: state}
}

func (s *keysStep) title() string { return "Import API Keys" }
func (s *keysStep) subtitle() string {
	if len(s.state.FoundKeys) == 0 {
		return "No .env files found to import"
	}
	return fmt.Sprintf("Found %d keys in .env files — select which to import into Vault", len(s.state.FoundKeys))
}

func (s *keysStep) Init() tea.Cmd {
	// Scan all found .env files
	s.state.FoundKeys = nil
	for _, f := range s.state.DotEnvFiles {
		kvs, err := parseDotEnv(f)
		if err != nil {
			continue
		}
		for k, v := range kvs {
			s.state.FoundKeys = append(s.state.FoundKeys, EnvKey{
				File:   f,
				Name:   k,
				Value:  v,
				Masked: fmt.Sprintf("%s = %s", k, maskSecret(v)),
			})
		}
	}

	if len(s.state.FoundKeys) == 0 {
		s.phase = keysPhaseSkip
		return nil
	}

	// Build multi-select
	items := make([]string, len(s.state.FoundKeys))
	for i, k := range s.state.FoundKeys {
		items[i] = k.Masked
	}
	s.ms = newMultiSelect(items)
	s.ms.SelectAll() // default: select all
	s.phase = keysPhaseSelect
	return nil
}

func (s *keysStep) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case keysImportedMsg:
		s.phase = keysPhaseDone
		s.imported = msg.count
		return s, nil
	case keysErrorMsg:
		s.phase = keysPhaseDone
		s.errMsg = msg.err
		return s, nil
	case tea.KeyMsg:
		switch msg.String() {
		case "enter":
			if s.phase == keysPhaseSelect {
				selected := s.ms.Selected()
				if len(selected) == 0 {
					s.phase = keysPhaseSkip
					return s, nil
				}
				// Map selected indices back to key names
				s.state.SelectedKeys = make([]string, 0)
				for _, label := range selected {
					for _, k := range s.state.FoundKeys {
						if k.Masked == label {
							s.state.SelectedKeys = append(s.state.SelectedKeys, k.Name)
							break
						}
					}
				}
				s.phase = keysPhaseImporting
				return s, s.importKeys()
			}
			if s.phase == keysPhaseDone || s.phase == keysPhaseSkip {
				return s, stepDone()
			}
		case "s":
			if s.phase == keysPhaseSelect {
				s.phase = keysPhaseSkip
				return s, nil
			}
		}
	}

	if s.phase == keysPhaseSelect {
		if km, ok := msg.(tea.KeyMsg); ok {
			s.ms.Update(km)
		}
	}
	return s, nil
}

func (s *keysStep) View() string {
	var sb strings.Builder
	sb.WriteString("\n")

	switch s.phase {
	case keysPhaseSkip:
		sb.WriteString(warnStyle.Render("  ⊘ Skipped") + " — no keys to import\n")
		sb.WriteString("\n" + hintStyle.Render("  Press Enter to continue"))

	case keysPhaseSelect:
		sb.WriteString("  Select keys to import into Vault:\n\n")
		sb.WriteString(s.ms.View())
		sb.WriteString(hintStyle.Render("\n  enter: import selected  ·  s: skip this step"))

	case keysPhaseImporting:
		sb.WriteString("  " + lipgloss.NewStyle().Foreground(accent).Render("⠋") +
			fmt.Sprintf(" Importing %d keys into Vault...\n", len(s.state.SelectedKeys)))

	case keysPhaseDone:
		if s.errMsg != "" {
			sb.WriteString("  " + crossStyle.Render("✗ Import failed: "+s.errMsg) + "\n")
		} else {
			sb.WriteString("  " + checkStyle.Render(fmt.Sprintf("✓ Imported %d key(s) into Vault", s.imported)) + "\n")
		}
		sb.WriteString("\n" + hintStyle.Render("  Press Enter to continue"))
	}

	return sb.String()
}

type keysImportedMsg struct{ count int }
type keysErrorMsg struct{ err string }

func (s *keysStep) importKeys() tea.Cmd {
	return func() tea.Msg {
		tc, err := NewMCPClient()
		if err != nil {
			return keysErrorMsg{err: err.Error()}
		}
		defer tc.Close()

		// First unlock vault
		_, err = tc.CallTool("vault_unlock", map[string]interface{}{
			"password": s.state.Password,
		})
		if err != nil {
			return keysErrorMsg{err: "vault unlock: " + err.Error()}
		}

		count := 0
		for _, name := range s.state.SelectedKeys {
			// Find the value
			var value string
			for _, k := range s.state.FoundKeys {
				if k.Name == name {
					value = k.Value
					break
				}
			}
			if value == "" {
				continue
			}
			_, err := tc.CallTool("vault_set", map[string]interface{}{
				"name":        name,
				"value":       value,
				"secret_type": "api_key",
			})
			if err != nil {
				return keysErrorMsg{err: fmt.Sprintf("failed to set %s: %s", name, err.Error())}
			}
			count++
		}
		return keysImportedMsg{count: count}
	}
}
