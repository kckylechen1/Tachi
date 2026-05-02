package main

import (
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

func (s *shellStep) title() string    { return T("Shell Integration", "Shell 集成") }
func (s *shellStep) subtitle() string { return T("Add tachi_env helper to your shell rc", "将 tachi_env 辅助函数添加到 shell rc") }

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
		helperFunc := `tachi_env() { eval "$(tachi env)"; }`
		sb.WriteString("  " + T("The following will be appended to", "以下内容将追加到") + " ")
		sb.WriteString(lipgloss.NewStyle().Foreground(accent).Bold(true).Render(shortPath(s.state.ShellRC)))
		sb.WriteString(":\n\n")
		sb.WriteString("  " + codeStyle.Render(helperFunc))
		sb.WriteString("\n\n")
		sb.WriteString("  " + T("Run tachi_env whenever you need secrets loaded.", "需要加载密钥时运行 tachi_env。") + "\n")
		sb.WriteString("  " + T("No password prompt on every terminal start.", "每次打开终端无需输入密码。") + "\n\n")
		sb.WriteString(hintStyle.Render("  " + T("enter/y: add to "+s.state.ShellType+"rc  ·  n: skip", "回车/y: 添加到 "+s.state.ShellType+"rc  ·  n: 跳过")))

	case shellPhaseWriting:
		sb.WriteString("  " + lipgloss.NewStyle().Foreground(accent).Render("⠋") + " " + T("Writing to", "正在写入") + " " + shortPath(s.state.ShellRC) + "...\n")

	case shellPhaseSkip:
		sb.WriteString(warnStyle.Render("  ⊘ "+T("Skipped", "已跳过")) + T(" — shell rc not modified\n", " — shell rc 未修改\n"))
		sb.WriteString("\n  " + T("You can add it manually later:", "您可以稍后手动添加:") + "\n")
		sb.WriteString("  " + codeStyle.Render(`tachi_env() { eval "$(tachi env)"; }`))
		sb.WriteString("\n\n" + hintStyle.Render(T("  Press Enter to continue", "  按回车继续")))

	case shellPhaseDone:
		if s.errMsg != "" {
			sb.WriteString("  " + crossStyle.Render("✗ "+s.errMsg) + "\n")
			sb.WriteString("\n  " + T("Add manually:", "手动添加:") + "\n")
			sb.WriteString("  " + codeStyle.Render(`tachi_env() { eval "$(tachi env)"; }`))
		} else {
			sb.WriteString("  " + checkStyle.Render("✓ "+T("Helper function added to", "辅助函数已添加到")+" "+shortPath(s.state.ShellRC)) + "\n")
			sb.WriteString("\n  " + T("Activate now:", "立即激活:") + "\n")
			sb.WriteString("  " + codeStyle.Render(`source `+s.state.ShellRC))
			sb.WriteString("\n  " + T("Then run:", "然后运行:") + "\n")
			sb.WriteString("  " + codeStyle.Render(`tachi_env`))
		}
		sb.WriteString("\n\n" + hintStyle.Render(T("  Press Enter to continue", "  按回车继续")))
	}

	return sb.String()
}

type shellWrittenMsg struct{}
type shellErrorMsg struct{ err string }

func (s *shellStep) writeRC() tea.Cmd {
	return func() tea.Msg {
		if err := appendToRC(s.state.ShellRC); err != nil {
			return shellErrorMsg{err: err.Error()}
		}
		return shellWrittenMsg{}
	}
}
