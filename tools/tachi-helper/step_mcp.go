package main

import (
	"fmt"
	"os/exec"
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
	state      *State
	phase      mcpPhase
	ms         *multiSelect
	errMsg     string
	registered int
}

func newMCPStep(state *State) *mcpStep {
	return &mcpStep{state: state}
}

func (s *mcpStep) title() string { return T("MCP Servers", "MCP 服务器") }
func (s *mcpStep) subtitle() string {
	return T("Register MCP servers in the Tachi Hub for all agents to use", "在 Tachi Hub 中注册 MCP 服务器供所有代理使用")
}

func (s *mcpStep) Init() tea.Cmd {
	items := make([]string, len(mcpServerDefs))
	for i, mcp := range mcpServerDefs {
		label := mcp.Name
		if mcp.NeedsKey != "" {
			label += fmt.Sprintf(T(" (requires %s)", " (需要 %s)"), mcp.NeedsKey)
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
		sb.WriteString("  " + T("Select MCP servers to register:", "选择要注册的 MCP 服务器:") + "\n\n")
		sb.WriteString(s.ms.View())
		sb.WriteString(hintStyle.Render(T("\n  enter: register selected  ·  s: skip", "\n  回车: 注册选中  ·  s: 跳过")))

	case mcpPhaseRegistering:
		sb.WriteString("  " + lipgloss.NewStyle().Foreground(accent).Render("⠋") +
			fmt.Sprintf(T(" Registering %d MCP server(s)...", " 正在注册 %d 个 MCP 服务器..."), len(s.state.SelectedMCPs))+"\n")

	case mcpPhaseSkip:
		sb.WriteString(warnStyle.Render("  ⊘ "+T("Skipped", "已跳过")) + T(" — no MCP servers registered\n", " — 未注册 MCP 服务器\n"))
		sb.WriteString("\n" + hintStyle.Render(T("  Press Enter to continue", "  按回车继续")))

	case mcpPhaseDone:
		if s.errMsg != "" {
			sb.WriteString("  " + crossStyle.Render("✗ "+T("Registration failed: ", "注册失败: ")+s.errMsg) + "\n")
		} else {
			sb.WriteString("  " + checkStyle.Render(fmt.Sprintf("✓ "+T("Registered %d MCP server(s)", "已注册 %d 个 MCP 服务器"), s.registered)) + "\n")
		}
		sb.WriteString("\n" + hintStyle.Render(T("  Press Enter to continue", "  按回车继续")))
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
					itemLabel += fmt.Sprintf(T(" (requires %s)", " (需要 %s)"), d.NeedsKey)
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
	cmd := exec.Command("tachi", "hub", "register",
		id,
		"--cap-type", "mcp",
		"--name", name,
		"--definition", definition,
	)
	output, err := cmd.CombinedOutput()
	if err != nil {
		return fmt.Errorf("%s: %s", err, strings.TrimSpace(string(output)))
	}
	return nil
}
