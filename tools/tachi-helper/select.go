package main

import (
	"fmt"
	"strings"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

// multiSelect is a reusable multi-select list component.
type multiSelect struct {
	items    []string
	selected map[int]bool
	cursor   int
}

func newMultiSelect(items []string) *multiSelect {
	return &multiSelect{
		items:    items,
		selected: make(map[int]bool),
	}
}

func (m *multiSelect) SelectAll() {
	for i := range m.items {
		m.selected[i] = true
	}
}

func (m *multiSelect) Selected() []string {
	var result []string
	for i, item := range m.items {
		if m.selected[i] {
			result = append(result, item)
		}
	}
	return result
}

func (m *multiSelect) Update(msg tea.KeyMsg) {
	switch msg.String() {
	case "up", "k":
		if m.cursor > 0 {
			m.cursor--
		}
	case "down", "j":
		if m.cursor < len(m.items)-1 {
			m.cursor++
		}
	case " ":
		if m.selected[m.cursor] {
			delete(m.selected, m.cursor)
		} else {
			m.selected[m.cursor] = true
		}
	case "a":
		if len(m.selected) == len(m.items) {
			m.selected = make(map[int]bool)
		} else {
			m.SelectAll()
		}
	}
}

func (m *multiSelect) View() string {
	var sb strings.Builder
	for i, item := range m.items {
		cursor := "  "
		if i == m.cursor {
			cursor = lipgloss.NewStyle().Foreground(accent).Render("▐ ")
		}

		checkbox := "[ ]"
		if m.selected[i] {
			checkbox = checkedStyle.Render("[✓]")
		} else {
			checkbox = lipgloss.NewStyle().Foreground(textDim).Render("[ ]")
		}

		label := itemStyle.Render(item)
		if i == m.cursor {
			label = lipgloss.NewStyle().Foreground(textBright).Bold(true).Render(item)
		}

		line := fmt.Sprintf("%s%s %s", cursor, checkbox, label)
		if i == m.cursor {
			line = lipgloss.NewStyle().Background(bgActive).PaddingRight(2).Render(line)
		}

		sb.WriteString("  " + line + "\n")
	}
	sb.WriteString(hintStyle.Render(T("\n  ↑/↓: navigate  ·  space: toggle  ·  a: select all", "\n  ↑/↓: 导航  ·  空格: 切换  ·  a: 全选")))
	return sb.String()
}
