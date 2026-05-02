package main

import "github.com/charmbracelet/lipgloss"

var (
	accent   = lipgloss.Color("#7C3AED")
	success  = lipgloss.Color("#10B981")
	warning  = lipgloss.Color("#F59E0B")
	danger   = lipgloss.Color("#EF4444")
	dimText  = lipgloss.Color("#6B7280")
	bright   = lipgloss.Color("#F9FAFB")
	normal   = lipgloss.Color("#E5E7EB")
	selected = lipgloss.Color("#818CF8")
)

var (
	titleStyle = lipgloss.NewStyle().
			Bold(true).
			Foreground(bright).
			Background(accent).
			Padding(0, 2)

	subtitleStyle = lipgloss.NewStyle().
			Foreground(normal).
			MarginBottom(1)

	headerBarStyle = lipgloss.NewStyle().
			Foreground(accent).
			Bold(true)

	stepDoneStyle = lipgloss.NewStyle().Foreground(success)
	stepActiveStyle = lipgloss.NewStyle().Foreground(accent).Bold(true)
	stepPendingStyle = lipgloss.NewStyle().Foreground(dimText)

	checkStyle = lipgloss.NewStyle().Foreground(success).Bold(true)
	crossStyle = lipgloss.NewStyle().Foreground(danger).Bold(true)
	warnStyle  = lipgloss.NewStyle().Foreground(warning).Bold(true)

	hintStyle = lipgloss.NewStyle().Foreground(dimText)

	cursorStyle = lipgloss.NewStyle().Foreground(selected).Bold(true)
	itemStyle   = lipgloss.NewStyle().Foreground(normal)
	checkedStyle = lipgloss.NewStyle().Foreground(success)

	boxStyle = lipgloss.NewStyle().
			Border(lipgloss.RoundedBorder()).
			BorderForeground(lipgloss.Color("#374151")).
			Padding(1, 2)

	inputPromptStyle = lipgloss.NewStyle().Foreground(accent).Bold(true)
	inputCursorStyle = lipgloss.NewStyle().Foreground(accent)

	buttonStyle = lipgloss.NewStyle().
			Foreground(bright).
			Background(accent).
			Padding(0, 2)

	buttonDimStyle = lipgloss.NewStyle().
			Foreground(dimText).
			Border(lipgloss.RoundedBorder()).
			BorderForeground(dimText).
			Padding(0, 2)
)
