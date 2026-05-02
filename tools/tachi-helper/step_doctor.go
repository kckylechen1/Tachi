package main

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

type doctorStep struct {
	state  *State
	done   bool
	checks []checkResult
}

type checkResult struct {
	label  string
	ok     bool
	detail string
}

func newDoctorStep(state *State) *doctorStep {
	return &doctorStep{state: state}
}

func (s *doctorStep) title() string    { return T("Health Check", "健康检查") }
func (s *doctorStep) subtitle() string { return T("Checking Tachi installation and configuration", "正在检查 Tachi 安装和配置") }

func (s *doctorStep) Init() tea.Cmd {
	return runDoctorChecks(s.state)
}

func (s *doctorStep) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case doctorResultsMsg:
		s.checks = msg.results
		s.done = true
		return s, nil
	case tea.KeyMsg:
		if s.done && (msg.String() == "enter" || msg.String() == " ") {
			if s.state.TachiPath == "" {
				return s, nil
			}
			return s, stepDone()
		}
	}
	return s, nil
}

func (s *doctorStep) View() string {
	if len(s.checks) == 0 {
		return fmt.Sprintf("\n  %s "+T("Running checks...", "正在运行检查...")+"\n",
			lipgloss.NewStyle().Foreground(accent).Render("⠋"))
	}

	var sb strings.Builder
	sb.WriteString("\n")
	for _, c := range s.checks {
		icon := checkStyle.Render("✓")
		if !c.ok {
			icon = crossStyle.Render("✗")
		}
		line := fmt.Sprintf("  %s %s", icon, c.label)
		if c.detail != "" {
			line += lipgloss.NewStyle().Foreground(textDim).Render("  " + c.detail)
		}
		sb.WriteString(line + "\n")
	}

	if s.done {
		if s.state.TachiPath == "" {
			sb.WriteString("\n" + crossStyle.Render("  " + T("Cannot continue without Tachi. Please install it and restart.", "缺少 Tachi 依赖，无法继续。请安装后重新运行。")))
		} else {
			sb.WriteString("\n" + hintStyle.Render(T("  Press Enter to continue", "  按回车键继续")))
		}
	}
	return sb.String()
}

type doctorResultsMsg struct {
	results []checkResult
}

func runDoctorChecks(state *State) tea.Cmd {
	return func() tea.Msg {
		var results []checkResult

		// Check 1: tachi binary
		path, err := findTachi()
		if err != nil {
			results = append(results, checkResult{
				label:  T("tachi binary", "tachi 二进制文件"),
				ok:     false,
				detail: T("not found in PATH — install tachi first", "在 PATH 中未找到 — 请先安装 tachi"),
			})
			state.TachiMissing = true
		} else {
			ver := tachiVersion()
			results = append(results, checkResult{
				label:  T("tachi binary", "tachi 二进制文件"),
				ok:     true,
				detail: fmt.Sprintf("%s (%s)", path, ver),
			})
			state.TachiPath = path
			state.TachiMissing = false
			state.TachiVersion = ver
			state.TachiMissing = false
		}

		// Check 2: global DB
		home, _ := os.UserHomeDir()
		dbPath := filepath.Join(home, ".tachi", "global", "memory.db")
		if _, err := os.Stat(dbPath); err == nil {
			state.GlobalDBPath = dbPath
			results = append(results, checkResult{
				label:  T("Global database", "全局数据库"),
				ok:     true,
				detail: shortPath(dbPath),
			})
		} else {
			results = append(results, checkResult{
				label:  T("Global database", "全局数据库"),
				ok:     false,
				detail: T("not found — will be created on first run", "未找到 — 首次运行时将自动创建"),
			})
		}

		// Check 3: vault
		state.VaultInit = false
		state.VaultLocked = true
		results = append(results, checkResult{
			label:  T("Vault", "密钥库"),
			ok:     false,
			detail: T("not initialized — will set up next", "未初始化 — 下一步将进行设置"),
		})

		// Check 4: .env files
		envFiles := scanDotEnvFiles()
		state.DotEnvFiles = envFiles
		if len(envFiles) > 0 {
			results = append(results, checkResult{
				label:  fmt.Sprintf(T(".env files (%d found)", "找到 %d 个 .env 文件"), len(envFiles)),
				ok:     true,
				detail: strings.Join(mapStr(envFiles, shortPath), ", "),
			})
		} else {
			results = append(results, checkResult{
				label:  T(".env files", ".env 文件"),
				ok:     true,
				detail: T("none found — clean setup", "未找到 — 全新安装"),
			})
		}

		// Check 5: shell
		shellType, rcPath := detectShell()
		state.ShellType = shellType
		state.ShellRC = rcPath
		results = append(results, checkResult{
			label:  fmt.Sprintf(T("Shell (%s)", "Shell (%s)"), shellType),
			ok:     true,
			detail: shortPath(rcPath),
		})

		return doctorResultsMsg{results: results}
	}
}

func shortPath(path string) string {
	home, _ := os.UserHomeDir()
	if home != "" && strings.HasPrefix(path, home) {
		return "~" + path[len(home):]
	}
	return path
}

func mapStr(slice []string, fn func(string) string) []string {
	out := make([]string, len(slice))
	for i, s := range slice {
		out[i] = fn(s)
	}
	return out
}

