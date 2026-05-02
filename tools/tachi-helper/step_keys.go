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
	state    *State
	phase    keysPhase
	ms       *multiSelect
	errMsg   string
	imported int
	skipped  int
}

func newKeysStep(state *State) *keysStep {
	return &keysStep{state: state}
}

func (s *keysStep) title() string { return T("Import API Keys", "导入 API 密钥") }
func (s *keysStep) subtitle() string {
	if len(s.state.FoundKeys) == 0 {
		return T("No .env files found to import", "未找到可导入的 .env 文件")
	}
	return fmt.Sprintf(T("Found %d keys in .env files — select which to import into Vault", "在 .env 文件中找到 %d 个密钥 — 选择要导入密钥库的密钥"), len(s.state.FoundKeys))
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
		sb.WriteString(warnStyle.Render("  ⊘ "+T("Skipped", "已跳过")) + T(" — no keys to import\n", " — 没有密钥需要导入\n"))
		sb.WriteString("\n" + hintStyle.Render(T("  Press Enter to continue", "  按回车继续")))

	case keysPhaseSelect:
		sb.WriteString("  " + T("Select keys to import into Vault:", "选择要导入密钥库的密钥:") + "\n\n")
		sb.WriteString(s.ms.View())
		sb.WriteString(hintStyle.Render(T("\n  enter: import selected  ·  s: skip this step", "\n  回车: 导入选中  ·  s: 跳过此步骤")))

	case keysPhaseImporting:
		sb.WriteString("  " + lipgloss.NewStyle().Foreground(accent).Render("⠋") +
			fmt.Sprintf(T(" Importing %d keys into Vault...", " 正在导入 %d 个密钥到密钥库..."), len(s.state.SelectedKeys))+"\n")

	case keysPhaseDone:
		if s.errMsg != "" {
			sb.WriteString("  " + crossStyle.Render("✗ "+T("Import failed: ", "导入失败: ")+s.errMsg) + "\n")
		} else {
			sb.WriteString("  " + checkStyle.Render(fmt.Sprintf("✓ "+T("Imported %d key(s) into Vault", "已导入 %d 个密钥到密钥库"), s.imported)) + "\n")
		}
		sb.WriteString("\n" + hintStyle.Render(T("  Press Enter to continue", "  按回车继续")))
	}

	return sb.String()
}

type keysImportedMsg struct{ count int }
type keysErrorMsg struct{ err string }

func (s *keysStep) importKeys() tea.Cmd {
	return func() tea.Msg {
		password, err := readFromKeychain()
		if err != nil {
			return keysErrorMsg{err: T("Keychain read failed: ", "钥匙串读取失败: ") + err.Error()}
		}

		tc, err := NewMCPClient()
		if err != nil {
			return keysErrorMsg{err: err.Error()}
		}
		defer tc.Close()

		// First unlock vault
		_, err = tc.CallTool("vault_unlock", map[string]interface{}{
			"password": password,
		})
		if err != nil {
			return keysErrorMsg{err: T("vault unlock: ", "密钥库解锁: ") + err.Error()}
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
				return keysErrorMsg{err: fmt.Sprintf(T("failed to set %s: ", "设置 %s 失败: "), name) + err.Error()}
			}
			count++
		}
		return keysImportedMsg{count: count}
	}
}
