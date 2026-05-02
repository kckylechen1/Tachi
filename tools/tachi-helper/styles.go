package main

import "github.com/charmbracelet/lipgloss"

var (
	// Brand Colors
	accent   = lipgloss.Color("#06B6D4") // Cyan
	success  = lipgloss.Color("#10B981") // Green
	warning  = lipgloss.Color("#F59E0B") // Amber
	danger   = lipgloss.Color("#EF4444") // Red

	// Grayscale
	textBright = lipgloss.Color("#FAFAFA") // near white
	textNormal = lipgloss.Color("#E4E4E7") // zinc-200
	textDim    = lipgloss.Color("#71717A") // zinc-500
	textMuted  = lipgloss.Color("#A1A1AA") // zinc-400

	// Backgrounds & Borders
	bgDark   = lipgloss.Color("#18181B") // zinc-900 (code blocks)
	bgActive = lipgloss.Color("#27272A") // zinc-800 (selected items)
	border   = lipgloss.Color("#3F3F46") // zinc-700
)

var (
	// 1. Layout & Border
	boxStyle = lipgloss.NewStyle().
			Border(lipgloss.RoundedBorder()).
			BorderForeground(border).
			Padding(1, 3).
			MarginTop(1)

	// 2. Header
	headerStyle = lipgloss.NewStyle().
			Foreground(textBright).
			Bold(true)

	stepBadgeStyle = lipgloss.NewStyle().
			Background(accent).
			Foreground(lipgloss.Color("#000000")).
			Bold(true).
			Padding(0, 1).
			MarginRight(1)

	dividerStyle = lipgloss.NewStyle().Foreground(border)

	subtitleStyle = lipgloss.NewStyle().Foreground(textDim)

	// 3. Step Progress Bar
	stepDoneStyle    = lipgloss.NewStyle().Foreground(success)
	stepActiveStyle  = lipgloss.NewStyle().Foreground(accent).Bold(true)
	stepPendingStyle = lipgloss.NewStyle().Foreground(textDim)

	// 4. Check Results
	checkStyle = lipgloss.NewStyle().Foreground(success).Bold(true)
	crossStyle = lipgloss.NewStyle().Foreground(danger).Bold(true)
	warnStyle  = lipgloss.NewStyle().Foreground(warning).Bold(true)

	// 5. Inputs & Cursor
	cursorStyle = lipgloss.NewStyle().Foreground(accent).Bold(true)

	// 6. Lists
	itemStyle    = lipgloss.NewStyle().Foreground(textNormal)
	checkedStyle = lipgloss.NewStyle().Foreground(success)

	activeItemStyle = lipgloss.NewStyle().
			Foreground(accent).
			Background(bgActive).
			Bold(true).
			PaddingRight(1)

	// 7. Code / Commands
	codeStyle = lipgloss.NewStyle().
			Foreground(textMuted).
			Background(bgDark).
			Padding(0, 1)

	// 8. Hints
	hintStyle = lipgloss.NewStyle().Foreground(textDim)

	// Key-Value pairs
	labelDimStyle = lipgloss.NewStyle().Foreground(textDim)
	valueStyle    = lipgloss.NewStyle().Foreground(textBright).Bold(true)
)
