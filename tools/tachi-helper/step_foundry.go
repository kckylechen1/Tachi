package main

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

type foundryPhase int

const (
	foundryPhaseSelect foundryPhase = iota
	foundryPhaseDone
)

// ---------- provider presets ----------

type providerPreset struct {
	name    string
	desc    string
	apiEnv  string // env var name for the API key (empty = no key needed)
	baseURL string // value to write (empty = not applicable, e.g. Voyage SDK)
	model   string // value to write
}

type modelLane struct {
	id       string // short id used in state map
	label    string
	presets  []providerPreset
	selected int
}

var defaultLanes = []modelLane{
	{
		id:    "embedding",
		label: "Embedding",
		presets: []providerPreset{
			{"Voyage voyage-4", T("Default, high-quality embedding", "默认，高质量 embedding"), "VOYAGE_API_KEY", "", "voyage-4"},
		},
		selected: 0,
	},
	{
		id:    "rerank",
		label: "Rerank",
		presets: []providerPreset{
			{"Voyage rerank-2.5", T("High-quality reranker", "高质量 reranker"), "VOYAGE_API_KEY", "", "rerank-2.5"},
			{T("Disabled", "关闭"), T("Skip rerank, use vector similarity only", "跳过 rerank，仅用向量相似度"), "", "", ""},
		},
		selected: 0,
	},
	{
		id:    "frontend_llm",
		label: T("Frontend LLM (extraction, hub skill, scan)", "前台 LLM（提取、技能、扫描）"),
		presets: []providerPreset{
			{"Qwen/Qwen3.5-27B (SiliconFlow)", T("Default, free tier, 27B can run local", "默认，有免费额度，27B 可本地跑"), "SILICONFLOW_API_KEY", "https://api.siliconflow.cn/v1/chat/completions", "Qwen/Qwen3.5-27B"},
			{"DeepSeek V4 Flash", T("Fast, cheap, strong", "快速、便宜、强力"), "DEEPSEEK_API_KEY", "https://api.deepseek.com/v1/chat/completions", "deepseek-v4-flash"},
			{"GLM-5.1 (ZhipuAI)", T("Strong reasoning", "推理能力强"), "ZAI_API_KEY", "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions", "glm-5.1"},
		},
		selected: 0,
	},
	{
		id:    "foundry_llm",
		label: T("Foundry LLM (distill, evolve, eval)", "后台 LLM（蒸馏、进化、评估）"),
		presets: []providerPreset{
			{"DeepSeek V4 Flash", T("Fast, cheap, strong — recommended", "快速、便宜、强力 — 推荐"), "DEEPSEEK_API_KEY", "https://api.deepseek.com/v1/chat/completions", "deepseek-v4-flash"},
			{"MiniMax-M2.7", T("High score on distill + audit in OpenClaw harness", "OpenClaw harness 蒸馏+审计高分"), "MINIMAX_API_KEY", "https://api.minimaxi.com/v1/chat/completions", "MiniMax-M2.7"},
			{"GLM-5.1 (ZhipuAI)", T("Strong reasoning, high eval scores", "推理强，eval 高分"), "ZAI_API_KEY", "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions", "glm-5.1"},
			{"Qwen/Qwen3.5-27B (SiliconFlow)", T("Budget option, free tier", "经济选择，有免费额度"), "SILICONFLOW_API_KEY", "https://api.siliconflow.cn/v1/chat/completions", "Qwen/Qwen3.5-27B"},
		},
		selected: 0,
	},
}

// ---------- step ----------

type foundryStep struct {
	state  *State
	phase  foundryPhase
	lanes  []modelLane
	cursor int
}

func newFoundryStep(state *State) *foundryStep {
	lanes := make([]modelLane, len(defaultLanes))
	copy(lanes, defaultLanes)
	return &foundryStep{state: state, lanes: lanes}
}

func (s *foundryStep) title() string {
	return T("Backend", "后台")
}

func (s *foundryStep) subtitle() string {
	return T(
		"Configure models for background tasks (embedding, rerank, LLM)",
		"配置后台任务使用的模型（embedding、rerank、LLM）",
	)
}

func (s *foundryStep) Init() tea.Cmd {
	s.phase = foundryPhaseSelect
	s.cursor = 0

	// Try to detect current selection from config.env
	for i := range s.lanes {
		s.lanes[i].selected = s.detectCurrent(s.lanes[i])
	}
	return nil
}

func (s *foundryStep) detectCurrent(lane modelLane) int {
	switch lane.id {
	case "frontend_llm":
		for _, key := range []string{"EXTRACT_MODEL", "SILICONFLOW_MODEL"} {
			if model, ok := readEnvKey(key); ok {
				for j, p := range lane.presets {
					if p.model == model {
						return j
					}
				}
			}
		}
	case "foundry_llm":
		for _, key := range []string{"DISTILL_MODEL", "REASONING_MODEL"} {
			if model, ok := readEnvKey(key); ok {
				for j, p := range lane.presets {
					if p.model == model {
						return j
					}
				}
			}
		}
	}
	return lane.selected
}

func (s *foundryStep) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	if s.phase == foundryPhaseDone {
		if km, ok := msg.(tea.KeyMsg); ok && km.String() == "enter" {
			return s, stepDone()
		}
		return s, nil
	}

	if km, ok := msg.(tea.KeyMsg); ok {
		switch km.String() {
		case "up", "k":
			if s.cursor > 0 {
				s.cursor--
			}
		case "down", "j":
			if s.cursor < len(s.lanes)-1 {
				s.cursor++
			}
		case "left", "h":
			l := &s.lanes[s.cursor]
			if l.selected > 0 {
				l.selected--
			}
		case "right", "l":
			l := &s.lanes[s.cursor]
			if l.selected < len(l.presets)-1 {
				l.selected++
			}
		case "enter":
			s.state.FoundrySelections = s.buildSelections()
			s.phase = foundryPhaseDone
			return s, s.writeConfigEnv()
		case "s":
			s.phase = foundryPhaseDone
			return s, nil
		}
	}
	return s, nil
}

func (s *foundryStep) View() string {
	var sb strings.Builder
	sb.WriteString("\n")

	if s.phase == foundryPhaseDone {
		for _, l := range s.lanes {
			p := l.presets[l.selected]
			sb.WriteString("  " + checkStyle.Render(fmt.Sprintf("✓ %s → %s", l.label, p.name)) + "\n")
		}
		// Show API key warnings
		missing := s.missingKeys()
		if len(missing) > 0 {
			sb.WriteString("\n")
			sb.WriteString("  " + warnStyle.Render(T("⚠ Missing API keys:", "⚠ 缺少 API key:")) + "\n")
			for _, key := range missing {
				sb.WriteString("    " + lipgloss.NewStyle().Foreground(textDim).Render("• "+key) + "\n")
			}
			sb.WriteString("    " + hintStyle.Render(T(
				"Add them to ~/.tachi/config.env or import via the Keys step",
				"添加到 ~/.tachi/config.env 或通过密钥步骤导入",
			)) + "\n")
		}
		sb.WriteString("\n" + hintStyle.Render(T("  Press Enter to continue", "  按回车继续")))
		return sb.String()
	}

	for i, l := range s.lanes {
		isCurrent := i == s.cursor
		p := l.presets[l.selected]

		// Lane label
		if isCurrent {
			sb.WriteString(lipgloss.NewStyle().Foreground(accent).Bold(true).Render("▐ "+l.label) + "\n")
		} else {
			sb.WriteString("  " + lipgloss.NewStyle().Foreground(textBright).Render(l.label) + "\n")
		}

		// Selected preset with ← → arrows
		arrow := ""
		if isCurrent {
			leftArrow := lipgloss.NewStyle().Foreground(textDim).Render("◀")
			rightArrow := lipgloss.NewStyle().Foreground(textDim).Render("▶")
			if l.selected == 0 {
				leftArrow = lipgloss.NewStyle().Foreground(textDim).Render(" ")
			}
			if l.selected == len(l.presets)-1 {
				rightArrow = lipgloss.NewStyle().Foreground(textDim).Render(" ")
			}
			arrow = fmt.Sprintf("    %s %s %s", leftArrow, lipgloss.NewStyle().Foreground(textBright).Bold(true).Render(p.name), rightArrow)
		} else {
			arrow = "      " + lipgloss.NewStyle().Foreground(textDim).Render(p.name)
		}
		sb.WriteString(arrow + "\n")

		// Description (only for current)
		if isCurrent {
			sb.WriteString("      " + lipgloss.NewStyle().Foreground(textDim).Italic(true).Render(p.desc) + "\n")
			if p.apiEnv != "" {
				keyStatus := lipgloss.NewStyle().Foreground(textDim).Render("key: " + p.apiEnv)
				if _, ok := readEnvKey(p.apiEnv); ok {
					keyStatus = checkStyle.Render("✓ " + p.apiEnv)
				} else {
					keyStatus = warnStyle.Render("✗ " + p.apiEnv + T(" (not configured)", "（未配置）"))
				}
				sb.WriteString("      " + keyStatus + "\n")
			}
		}
		sb.WriteString("\n")
	}

	sb.WriteString(hintStyle.Render(T(
		"  ↑/↓: select lane  ·  ←/→: change provider  ·  enter: confirm  ·  s: skip",
		"  ↑/↓: 选择通道  ·  ←/→: 切换供应商  ·  回车: 确认  ·  s: 跳过",
	)))

	return sb.String()
}

// ---------- config generation ----------

func (s *foundryStep) buildSelections() map[string]string {
	sel := make(map[string]string)
	for _, l := range s.lanes {
		p := l.presets[l.selected]
		sel[l.id] = p.name
	}
	return sel
}

// llmEnvEntries expands the unified LLM selection into per-lane env vars.
func (s *foundryStep) llmEnvEntries() map[string]string {
	entries := make(map[string]string)

	for _, l := range s.lanes {
		p := l.presets[l.selected]
		switch l.id {
		case "embedding":
			// Embedding uses Voyage SDK, no BASE_URL/MODEL env vars needed
			// VOYAGE_API_KEY is managed in the Keys step

		case "rerank":
			if p.model == "" {
				entries["VOYAGE_RERANK_ENABLED"] = "false"
			} else {
				entries["VOYAGE_RERANK_ENABLED"] = "true"
			}

		case "frontend_llm":
			// Frontend: extraction, hub skill, scan, summary (27B class)
			for _, lane := range []string{"EXTRACT", "SUMMARY"} {
				if p.apiEnv != "" {
					entries[lane+"_API_KEY"] = "$" + p.apiEnv
				}
				if p.baseURL != "" {
					entries[lane+"_BASE_URL"] = p.baseURL
				}
				if p.model != "" {
					entries[lane+"_MODEL"] = p.model
				}
			}

		case "foundry_llm":
			// Foundry: distill, reasoning, evolve, eval
			for _, lane := range []string{"DISTILL", "REASONING"} {
				if p.apiEnv != "" {
					entries[lane+"_API_KEY"] = "$" + p.apiEnv
				}
				if p.baseURL != "" {
					entries[lane+"_BASE_URL"] = p.baseURL
				}
				if p.model != "" {
					entries[lane+"_MODEL"] = p.model
				}
			}
		}
	}
	return entries
}

func (s *foundryStep) missingKeys() []string {
	seen := make(map[string]bool)
	var missing []string
	for _, l := range s.lanes {
		p := l.presets[l.selected]
		if p.apiEnv == "" || seen[p.apiEnv] {
			continue
		}
		seen[p.apiEnv] = true
		if _, ok := readEnvKey(p.apiEnv); !ok {
			missing = append(missing, p.apiEnv)
		}
	}
	return missing
}

type foundryWriteDoneMsg struct{ err string }

func (s *foundryStep) writeConfigEnv() tea.Cmd {
	entries := s.llmEnvEntries()

	return func() tea.Msg {
		if err := upsertConfigEnvKeys(entries); err != nil {
			return foundryWriteDoneMsg{err: err.Error()}
		}
		return foundryWriteDoneMsg{}
	}
}

// ---------- config.env helpers ----------

// readEnvKey reads a key from ~/.tachi/config.env without loading it into the process.
func readEnvKey(key string) (string, bool) {
	home, err := homeDir()
	if err != nil {
		return "", false
	}
	kvs, err := parseDotEnv(filepath.Join(home, ".tachi", "config.env"))
	if err != nil {
		return "", false
	}
	val, ok := kvs[key]
	return val, ok
}

// upsertConfigEnvKeys adds or updates keys in ~/.tachi/config.env.
func upsertConfigEnvKeys(entries map[string]string) error {
	home, err := homeDir()
	if err != nil {
		return err
	}
	configPath := filepath.Join(home, ".tachi", "config.env")

	existing, _ := os.ReadFile(configPath)
	lines := strings.Split(string(existing), "\n")

	updated := make(map[string]bool)
	for i, line := range lines {
		trimmed := strings.TrimSpace(line)
		if trimmed == "" || strings.HasPrefix(trimmed, "#") {
			continue
		}
		idx := strings.Index(trimmed, "=")
		if idx < 1 {
			continue
		}
		key := strings.TrimSpace(trimmed[:idx])
		if val, ok := entries[key]; ok {
			// Don't overwrite real API keys with $REF placeholders
			if strings.HasPrefix(val, "$") {
				updated[key] = true
				continue
			}
			lines[i] = key + "=" + val
			updated[key] = true
		}
	}

	// Append any keys not already in the file
	needsHeader := true
	for key, val := range entries {
		if updated[key] {
			continue
		}
		// Don't write $REF placeholders — those mean "use the same key as..."
		if strings.HasPrefix(val, "$") {
			continue
		}
		if needsHeader {
			lines = append(lines, "", "# Foundry model lane configuration")
			needsHeader = false
		}
		lines = append(lines, key+"="+val)
	}

	return os.WriteFile(configPath, []byte(strings.Join(lines, "\n")), 0644)
}

func homeDir() (string, error) {
	h, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("cannot determine home directory: %w", err)
	}
	return h, nil
}
