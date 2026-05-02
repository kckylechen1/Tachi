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

func (s *welcomeStep) title() string    { return T("Welcome", "欢迎") }
func (s *welcomeStep) subtitle() string { return "" }

func (s *welcomeStep) Init() tea.Cmd { return nil }

func (s *welcomeStep) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.KeyMsg:
		switch msg.String() {
		case "enter", " ":
			return s, stepDone()
		case "l", "L":
			isZh = !isZh
			return s, nil
		}
	}
	return s, nil
}

func (s *welcomeStep) View() string {
	logo := lipgloss.NewStyle().Foreground(accent).Bold(true).Render(
		strings.Join([]string{
			"",
			"  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━",
			"    " + T("Tachi Setup Wizard", "Tachi 设置向导"),
			"  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━",
			"",
		}, "\n"),
	)

	steps := []string{
		T("Check your Tachi installation", "检查 Tachi 安装"),
		T("Initialize the encrypted Vault", "初始化加密密钥库"),
		T("Import API keys from .env files", "从 .env 文件导入 API 密钥"),
		T("Register MCP servers", "注册 MCP 服务器"),
		T("Set up shell integration", "设置 Shell 集成"),
	}

	var stepList strings.Builder
	stepList.WriteString("\n")
	for i, s := range steps {
		num := lipgloss.NewStyle().Foreground(accent).Bold(true).Render(fmt.Sprintf("%d.", i+1))
		stepList.WriteString(fmt.Sprintf("  %s  %s\n", num, s))
	}

	desc := lipgloss.NewStyle().Foreground(textDim).Render(
		T("\n  Your API keys will be stored encrypted and never\n  committed to git.",
			"\n  您的 API 密钥将被加密存储，且永远不会\n  被提交到 git。"),
	)

	enter := lipgloss.NewStyle().Foreground(accent).Bold(true).Render(
		fmt.Sprintf(T("\n  Press %s to start", "\n  按下 %s 开始"), "Enter"),
	)

	return logo + stepList.String() + desc + enter
}
