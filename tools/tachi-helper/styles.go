package main

import "github.com/charmbracelet/lipgloss"

// OpenCode-inspired palette: foreground-only, minimal backgrounds.
var (
	accent   = lipgloss.Color("#06B6D4") // cyan
	success  = lipgloss.Color("#10B981") // green
	warning  = lipgloss.Color("#F59E0B") // amber
	danger   = lipgloss.Color("#EF4444") // red
	dimText  = lipgloss.Color("#71717A") // zinc-500
	bright   = lipgloss.Color("#FAFAFA") // near white
	normal   = lipgloss.Color("#E4E4E7") // zinc-200
	selected = lipgloss.Color("#22D3EE") // cyan-400
	muted    = lipgloss.Color("#A1A1AA") // zinc-400
)

var (
	subtitleStyle = lipgloss.NewStyle().
			Foreground(dimText).
			MarginBottom(1)

	headerStyle = lipgloss.NewStyle().
			Foreground(accent).
			Bold(true)

	stepDoneStyle    = lipgloss.NewStyle().Foreground(success)
	stepActiveStyle  = lipgloss.NewStyle().Foreground(accent).Bold(true)
	stepPendingStyle = lipgloss.NewStyle().Foreground(dimText)

	checkStyle = lipgloss.NewStyle().Foreground(success).Bold(true)
	crossStyle = lipgloss.NewStyle().Foreground(danger).Bold(true)
	warnStyle  = lipgloss.NewStyle().Foreground(warning).Bold(true)

	hintStyle = lipgloss.NewStyle().Foreground(dimText)

	cursorStyle  = lipgloss.NewStyle().Foreground(accent).Bold(true)
	itemStyle    = lipgloss.NewStyle().Foreground(normal)
	checkedStyle = lipgloss.NewStyle().Foreground(success)

	// No box background — let the terminal bg show through.
	// Just padding for breathing room.
	boxStyle = lipgloss.NewStyle().
			Padding(1, 3)

	// Code: just dim foreground, no background.
	codeStyle = lipgloss.NewStyle().Foreground(muted)

	// Labels in key-value pairs.
	labelDimStyle = lipgloss.NewStyle().Foreground(dimText)
	valueStyle    = lipgloss.NewStyle().Foreground(normal)
)
