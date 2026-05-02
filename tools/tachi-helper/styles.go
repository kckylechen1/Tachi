package main

import "github.com/charmbracelet/lipgloss"

var (
	// Primary: cyan/teal (OpenCode-inspired)
	accent = lipgloss.Color("#06B6D4")

	success  = lipgloss.Color("#10B981")
	warning  = lipgloss.Color("#F59E0B")
	danger   = lipgloss.Color("#EF4444")
	dimText  = lipgloss.Color("#71717A")
	bright   = lipgloss.Color("#FAFAFA")
	normal   = lipgloss.Color("#E4E4E7")
	selected = lipgloss.Color("#22D3EE")

	// Backgrounds
	bgDark    = lipgloss.Color("#0A0A0A")
	bgSurface = lipgloss.Color("#18181B")
	bgHigh    = lipgloss.Color("#27272A")
)

var (
	titleStyle = lipgloss.NewStyle().
			Bold(true).
			Foreground(bright).
			Background(lipgloss.Color("#164E63")).
			Padding(0, 2)

	subtitleStyle = lipgloss.NewStyle().
			Foreground(dimText).
			MarginBottom(1)

	headerBarStyle = lipgloss.NewStyle().
			Foreground(accent).
			Bold(true)

	stepDoneStyle   = lipgloss.NewStyle().Foreground(success)
	stepActiveStyle = lipgloss.NewStyle().Foreground(accent).Bold(true)
	stepPendingStyle = lipgloss.NewStyle().Foreground(dimText)

	checkStyle = lipgloss.NewStyle().Foreground(success).Bold(true)
	crossStyle = lipgloss.NewStyle().Foreground(danger).Bold(true)
	warnStyle  = lipgloss.NewStyle().Foreground(warning).Bold(true)

	hintStyle = lipgloss.NewStyle().Foreground(dimText)

	cursorStyle  = lipgloss.NewStyle().Foreground(accent).Bold(true)
	itemStyle    = lipgloss.NewStyle().Foreground(normal)
	checkedStyle = lipgloss.NewStyle().Foreground(success)

	boxStyle = lipgloss.NewStyle().
			Border(lipgloss.RoundedBorder()).
			BorderForeground(lipgloss.Color("#27272A")).
			Background(bgDark).
			Foreground(normal).
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
			BorderForeground(lipgloss.Color("#3F3F46")).
			Padding(0, 2)

	codeBlockStyle = lipgloss.NewStyle().
			Foreground(bright).
			Background(bgHigh).
			Padding(0, 1)

	labelDimStyle = lipgloss.NewStyle().Foreground(dimText)
	valueStyle    = lipgloss.NewStyle().Foreground(normal)
)
