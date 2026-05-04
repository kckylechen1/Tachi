package main

import (
	"fmt"
	"strings"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

type doneStep struct {
	state *State
}

func newDoneStep(state *State) *doneStep {
	return &doneStep{state: state}
}

func (s *doneStep) title() string    { return T("Setup Complete", "设置完成") }
func (s *doneStep) subtitle() string { return T("Summary of what was configured", "配置摘要") }

func (s *doneStep) Init() tea.Cmd { return nil }

func (s *doneStep) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	return s, nil
}

func (s *doneStep) View() string {
	var sb strings.Builder

	sb.WriteString("\n")
	sb.WriteString(checkStyle.Render("  ✓ "+T("Tachi setup complete!", "Tachi 设置完成!")) + "\n\n")
	sb.WriteString("  ─────────────────────────────────────\n\n")

	items := []struct {
		label string
		value string
	}{
		{T("Tachi", "Tachi"), s.state.TachiVersion},
		{T("Vault", "密钥库"), fmt.Sprintf(T("initialized (%d keys)", "已初始化 (%d 个密钥)"), len(s.state.SelectedKeys))},
		{T("Providers", "供应商"), providerSummary(s.state)},
		{T("Keys imported", "已导入密钥"), fmt.Sprintf("%d", len(s.state.SelectedKeys))},
		{T("MCP servers", "MCP 服务器"), fmt.Sprintf(T("%d registered", "已注册 %d 个"), len(s.state.SelectedMCPs))},
		{T("Backend", "后台"), foundryBackendSummary(s.state)},
		{T("Shell", "Shell"), fmt.Sprintf("%s (%s)", s.state.ShellType, boolMark(s.state.RCModified))},
	}

	for _, item := range items {
		sb.WriteString(fmt.Sprintf("  %-16s %s\n",
			labelDimStyle.Render(item.label+":"),
			valueStyle.Render(item.value),
		))
	}

	sb.WriteString("\n  ─────────────────────────────────────\n\n")
	sb.WriteString("  " + T("Next steps:", "后续步骤:") + "\n\n")
	sb.WriteString("  1. " + lipgloss.NewStyle().Foreground(textBright).Bold(true).Render(T("Load secrets into your shell:", "将密钥加载到 shell 中:")) + "\n")
	sb.WriteString("     " + codeStyle.Render(`tachi_env`) + "\n\n")
	sb.WriteString("  2. " + lipgloss.NewStyle().Foreground(textBright).Bold(true).Render(T("Store more keys via MCP:", "通过 MCP 存储更多密钥:")) + "\n")
	sb.WriteString("     " + codeStyle.Render(`tachi vault_set(name="KEY", value="secret")`) + "\n\n")
	sb.WriteString("  3. " + lipgloss.NewStyle().Foreground(textBright).Bold(true).Render(T("Remove .env files:", "移除 .env 文件:")) + "\n")
	sb.WriteString("     " + codeStyle.Render(`git rm .env && echo ".env" >> .gitignore`) + "\n")

	sb.WriteString("\n\n" + hintStyle.Render(T("  q / esc: quit", "  q / esc: 退出")))

	return sb.String()
}

func providerSummary(state *State) string {
	if len(state.ProviderSelections) == 0 {
		return T("defaults unchanged", "默认值未修改")
	}
	parts := make([]string, 0, 3)
	for _, key := range []string{"embedding", "reasoning", "agent"} {
		if sel, ok := state.ProviderSelections[key]; ok && sel.Name != "" {
			parts = append(parts, sel.Name)
		}
	}
	if len(parts) == 0 {
		return T("defaults unchanged", "默认值未修改")
	}
	return strings.Join(parts, " / ")
}

func foundryBackendSummary(state *State) string {
	if len(state.FoundrySelections) == 0 {
		return T("skipped", "已跳过")
	}
	fe := state.FoundrySelections["frontend_llm"]
	fo := state.FoundrySelections["foundry_llm"]
	if fe == "" && fo == "" {
		return T("skipped", "已跳过")
	}
	if fe == fo {
		return fe
	}
	return fmt.Sprintf(T("front: %s / foundry: %s", "前台: %s / 后台: %s"), fe, fo)
}

func boolMark(b bool) string {
	if b {
		return checkStyle.Render(T("helper added", "已添加辅助函数"))
	}
	return T("skipped", "已跳过")
}
