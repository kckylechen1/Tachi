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
	stepProviders
	stepKeys
	stepMCP
	stepFoundry
	stepShell
	stepSummary
	stepCount
)

type stepDoneMsg struct{}
type stepBackMsg struct{}

type wizard struct {
	current  stepID
	steps    []step
	state    *State
	mcp      *MCPClient
	width    int
	height   int
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
	w.steps[stepProviders] = newProvidersStep(state)
	w.steps[stepKeys] = newKeysStep(state)
	w.steps[stepMCP] = newMCPStep(state)
	w.steps[stepFoundry] = newFoundryStep(state)
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
			w.quitting = true
			return w, tea.Quit
		case "esc":
			if w.current == stepSummary {
				w.quitting = true
				return w, tea.Quit
			}
		}

	case stepDoneMsg:
		if w.current == stepDoctor && w.state.TachiMissing {
			return w, nil
		}
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

	wWidth := w.width - 4
	if wWidth > 80 {
		wWidth = 80 // Max width
	} else if wWidth < 50 {
		wWidth = 50 // Min width
	}

	return boxStyle.
		Width(wWidth).
		Height(max(w.height-4, 20)).
		Render(sb.String())
}

func (w *wizard) renderHeader(s step) string {
	badge := stepBadgeStyle.Render(fmt.Sprintf(" STEP %d/%d ", w.current+1, stepCount))
	title := headerStyle.Render(T("Tachi Setup Wizard", "Tachi 设置向导"))

	headerRow := lipgloss.JoinHorizontal(lipgloss.Center, badge, title)

	var steps []string
	labels := []string{
		T("Welcome", "欢迎"),
		T("Health", "健康检查"),
		T("Vault", "密钥库"),
		T("Providers", "供应商"),
		T("Keys", "密钥"),
		T("MCP", "MCP"),
		T("Backend", "后台"),
		T("Shell", "Shell"),
		T("Done", "完成"),
	}
	for i, label := range labels {
		switch {
		case stepID(i) < w.current:
			steps = append(steps, stepDoneStyle.Render("✓ "+label))
		case stepID(i) == w.current:
			steps = append(steps, stepActiveStyle.Render("▶ "+label))
		default:
			steps = append(steps, stepPendingStyle.Render("· "+label))
		}
	}
	progress := lipgloss.JoinHorizontal(lipgloss.Top, strings.Join(steps, stepPendingStyle.Render(" ─ ")))
	divider := dividerStyle.Render(strings.Repeat("─", max(min(w.width-8, 65), 30)))

	sub := subtitleStyle.Render(s.subtitle())

	headerBlock := headerRow + "\n\n" + progress + "\n" + divider
	if sub != "" {
		headerBlock += "\n\n" + sub
	}

	return headerBlock
}

func (w *wizard) renderFooter() string {
	var hint string
	if w.current == stepWelcome {
		hint = T("enter: start  ·  L: language  ·  q: quit", "回车: 开始  ·  L: 切换中英文  ·  q: 退出")
	} else if w.current == stepSummary {
		hint = T("q / esc: quit", "q / esc: 退出")
	} else {
		hint = T("enter: next  ·  esc: back  ·  q: quit", "回车: 下一步  ·  esc: 上一步  ·  q: 退出")
	}

	divider := dividerStyle.Render(strings.Repeat("─", max(min(w.width-8, 60), 30)))
	return "\n" + divider + "\n" + hintStyle.Render(hint)
}
