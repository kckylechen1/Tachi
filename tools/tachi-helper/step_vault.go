package main

import (
	"fmt"
	"strings"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

type vaultPhase int

const (
	vaultPhaseCreating vaultPhase = iota
	vaultPhaseDone
	vaultPhaseError
)

type vaultStep struct {
	state   *State
	phase   vaultPhase
	errMsg  string
	created bool
}

func newVaultStep(state *State) *vaultStep {
	return &vaultStep{
		state: state,
		phase: vaultPhaseCreating,
	}
}

func (s *vaultStep) title() string { return T("Vault Setup", "密钥库设置") }
func (s *vaultStep) subtitle() string {
	if s.state.VaultInit {
		return T("Unlocking existing encrypted Vault with Keychain", "正在使用钥匙串解锁已有加密密钥库")
	}
	return T("Creating encrypted Vault with auto-generated key", "正在使用自动生成的密钥创建加密密钥库")
}

func (s *vaultStep) Init() tea.Cmd {
	return vaultEnsureWithKeychainCmd(s.state)
}

func (s *vaultStep) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case vaultCreatedMsg:
		s.phase = vaultPhaseDone
		s.created = msg.created
		return s, nil
	case vaultErrorMsg:
		s.phase = vaultPhaseError
		s.errMsg = msg.err
		return s, nil
	case tea.KeyMsg:
		switch msg.String() {
		case "enter":
			if s.phase == vaultPhaseDone {
				return s, stepDone()
			}
			if s.phase == vaultPhaseError {
				s.phase = vaultPhaseCreating
				s.errMsg = ""
				return s, vaultEnsureWithKeychainCmd(s.state)
			}
		case "ctrl+c":
			return s, tea.Quit
		}
	}
	return s, nil
}

func (s *vaultStep) View() string {
	var sb strings.Builder
	sb.WriteString("\n")

	switch s.phase {
	case vaultPhaseCreating:
		if s.state.VaultInit {
			sb.WriteString("  " + lipgloss.NewStyle().Foreground(accent).Render("⠋") + " " + T("Reading Keychain password and unlocking vault...", "正在读取钥匙串密码并解锁密钥库...") + "\n")
		} else {
			sb.WriteString("  " + lipgloss.NewStyle().Foreground(accent).Render("⠋") + " " + T("Generating master key and initializing vault...", "正在生成主密钥并初始化密钥库...") + "\n")
			sb.WriteString("  " + lipgloss.NewStyle().Foreground(textDim).Render(T("Password will be stored in macOS Keychain (tachi-vault)", "密码将存储在 macOS 钥匙串 (tachi-vault)")) + "\n")
		}

	case vaultPhaseDone:
		if s.created {
			sb.WriteString("  " + checkStyle.Render("✓ "+T("Vault initialized successfully", "密钥库初始化成功")) + "\n")
		} else {
			sb.WriteString("  " + checkStyle.Render("✓ "+T("Vault unlocked successfully", "密钥库解锁成功")) + "\n")
		}
		sb.WriteString("  " + lipgloss.NewStyle().Foreground(textDim).Render(
			fmt.Sprintf(T("Master key stored in macOS Keychain (%s)", "主密钥已存储在 macOS 钥匙串 (%s)"), "tachi-vault")) + "\n")
		sb.WriteString("\n" + hintStyle.Render(T("  Press Enter to continue", "  按回车继续")))

	case vaultPhaseError:
		sb.WriteString("  " + crossStyle.Render("✗ "+T("Vault setup failed", "密钥库设置失败")) + "\n")
		sb.WriteString("  " + lipgloss.NewStyle().Foreground(textDim).Render(s.errMsg) + "\n")
		sb.WriteString("\n" + hintStyle.Render(T("  Press Enter to retry", "  按回车重试")))
	}

	return sb.String()
}

type vaultCreatedMsg struct {
	created bool
}
type vaultErrorMsg struct {
	err string
}

func vaultEnsureWithKeychainCmd(state *State) tea.Cmd {
	return func() tea.Msg {
		wasInitialized := state.VaultInit
		password, err := ensureVaultPassword(wasInitialized)
		if err != nil {
			return vaultErrorMsg{err: err.Error()}
		}

		tc, err := NewMCPClient()
		if err != nil {
			return vaultErrorMsg{err: T("Failed to start tachi: ", "启动 tachi 失败: ") + err.Error()}
		}
		defer tc.Close()

		tool := "vault_init"
		if wasInitialized {
			tool = "vault_unlock"
		}
		_, err = tc.CallTool(tool, map[string]interface{}{"password": password})
		if err != nil {
			return vaultErrorMsg{err: err.Error()}
		}
		state.VaultInit = true
		state.VaultLocked = false
		return vaultCreatedMsg{created: !wasInitialized}
	}
}

func ensureVaultPassword(initialized bool) (string, error) {
	if initialized {
		password, err := readFromKeychain()
		if err != nil {
			return "", fmt.Errorf("%s%s", T("existing Vault is initialized but Keychain password is missing: ", "已有密钥库已初始化，但钥匙串密码缺失: "), err.Error())
		}
		return password, nil
	}

	password := generatePassword()
	if err := storeInKeychain(password); err != nil {
		return "", fmt.Errorf("%s%s", T("Keychain store failed: ", "钥匙串存储失败: "), err.Error())
	}
	return password, nil
}
