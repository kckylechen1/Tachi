package main

import (
	"fmt"
	"net/url"
	"os"
	"path/filepath"
	"strings"

	"github.com/charmbracelet/bubbles/textinput"
	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

type providersPhase int

const (
	providersPhaseSelect providersPhase = iota
	providersPhaseCustomEndpoint
	providersPhaseWriting
	providersPhaseDone
	providersPhaseSkip
)

type providerOption struct {
	id          string
	name        string
	desc        string
	apiKeyEnv   string
	endpoint    string
	model       string
	isDefault   bool
	recommended bool
	requiresURL bool
}

type providerCategory struct {
	id       string
	label    string
	options  []providerOption
	selected int
}

var defaultProviderCategories = []providerCategory{
	{
		id:    "embedding",
		label: T("Embedding Provider", "Embedding 供应商"),
		options: []providerOption{
			{
				id:          "voyage",
				name:        "Voyage AI",
				desc:        T("Best default for vector search", "向量搜索的最佳默认选择"),
				apiKeyEnv:   "VOYAGE_API_KEY",
				endpoint:    "https://api.voyageai.com/v1/embeddings",
				model:       "voyage-4",
				isDefault:   true,
				recommended: true,
			},
			{
				id:        "siliconflow",
				name:      "Silicon Flow",
				desc:      T("OpenAI-compatible embedding endpoint", "OpenAI 兼容 embedding 端点"),
				apiKeyEnv: "SILICONFLOW_API_KEY",
				endpoint:  "https://api.siliconflow.cn/v1/embeddings",
				model:     "BAAI/bge-m3",
			},
			{
				id:          "openai-compatible",
				name:        T("OpenAI-compatible", "OpenAI 兼容"),
				desc:        T("Custom embedding endpoint URL", "自定义 embedding 端点 URL"),
				apiKeyEnv:   "EMBEDDING_API_KEY",
				model:       "text-embedding-3-large",
				requiresURL: true,
			},
		},
		selected: 0,
	},
	{
		id:    "reasoning",
		label: T("Reasoning / Chat LLM", "推理 / 聊天 LLM"),
		options: []providerOption{
			{
				id:          "claude-code",
				name:        "Claude Code agent",
				desc:        T("Default backend worker; falls back to chat lane when unavailable", "默认后台 worker；不可用时回退到聊天通道"),
				isDefault:   true,
				recommended: true,
			},
			{
				id:          "glm-5.1",
				name:        "GLM 5.1 in Claude agent",
				desc:        T("Recommended for review and analysis", "推荐用于 review 和分析"),
				apiKeyEnv:   "ZAI_API_KEY",
				endpoint:    "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions",
				model:       "glm-5.1",
				recommended: true,
			},
			{
				id:        "siliconflow",
				name:      "Silicon Flow (DeepSeek / Qwen)",
				desc:      T("Budget OpenAI-compatible chat provider", "经济型 OpenAI 兼容聊天供应商"),
				apiKeyEnv: "SILICONFLOW_API_KEY",
				endpoint:  "https://api.siliconflow.cn/v1/chat/completions",
				model:     "Qwen/Qwen3.5-27B",
			},
			{
				id:          "openai-compatible",
				name:        T("OpenAI-compatible", "OpenAI 兼容"),
				desc:        T("Custom chat completions endpoint URL", "自定义 chat completions 端点 URL"),
				apiKeyEnv:   "REASONING_API_KEY",
				model:       "gpt-4.1",
				requiresURL: true,
			},
		},
		selected: 0,
	},
	{
		id:    "agent",
		label: T("Agent Backend", "Agent 后端"),
		options: []providerOption{
			{
				id:          "claude-code",
				name:        "Claude Code",
				desc:        T("Default dispatch agent", "默认 dispatch agent"),
				isDefault:   true,
				recommended: true,
			},
			{
				id:    "codex",
				name:  "Codex",
				desc:  T("Use Codex CLI for delegated tasks", "使用 Codex CLI 执行委托任务"),
				model: "codex",
			},
			{
				id:        "glm-5.1-claude",
				name:      "GLM 5.1 via Claude agent",
				desc:      T("Claude Code dispatch with GLM model override", "Claude Code dispatch 搭配 GLM 模型覆盖"),
				apiKeyEnv: "ZAI_API_KEY",
				endpoint:  "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions",
				model:     "glm-5.1",
			},
			{
				id:    "custom",
				name:  T("Custom command", "自定义命令"),
				desc:  T("Set TACHI_AGENT_COMMAND manually for custom dispatch", "手动设置 TACHI_AGENT_COMMAND 用于自定义 dispatch"),
				model: "custom",
			},
		},
		selected: 0,
	},
}

type providersStep struct {
	state       *State
	phase       providersPhase
	categories  []providerCategory
	cursor      int
	input       textinput.Model
	pending     [][2]int
	pendingIdx  int
	customURLs  map[string]string
	writeErrMsg string
}

func newProvidersStep(state *State) *providersStep {
	categories := cloneProviderCategories(defaultProviderCategories)
	input := textinput.New()
	input.CharLimit = 300
	input.Width = 66
	input.Prompt = "  URL: "
	return &providersStep{
		state:      state,
		categories: categories,
		input:      input,
		customURLs: make(map[string]string),
	}
}

func cloneProviderCategories(src []providerCategory) []providerCategory {
	out := make([]providerCategory, len(src))
	for i, cat := range src {
		out[i] = cat
		out[i].options = append([]providerOption(nil), cat.options...)
	}
	return out
}

func (s *providersStep) title() string { return T("Providers", "供应商") }

func (s *providersStep) subtitle() string {
	return T(
		"Choose API providers and endpoints for embeddings, reasoning, and dispatch",
		"选择 embedding、推理和 dispatch 的 API 供应商与端点",
	)
}

func (s *providersStep) Init() tea.Cmd {
	s.phase = providersPhaseSelect
	s.cursor = 0
	s.pending = nil
	s.pendingIdx = 0
	s.writeErrMsg = ""
	s.input.Blur()
	for i := range s.categories {
		s.categories[i].selected = s.detectCurrentSelection(s.categories[i])
	}
	return nil
}

func (s *providersStep) detectCurrentSelection(cat providerCategory) int {
	selection, ok := s.state.ProviderSelections[cat.id]
	if ok {
		for i, opt := range cat.options {
			if opt.id == selection.ProviderID {
				return i
			}
		}
	}

	switch cat.id {
	case "embedding":
		return detectProviderByEnv(cat, []string{"EMBEDDING_PROVIDER"}, []string{"EMBEDDING_BASE_URL"}, []string{"EMBEDDING_MODEL"})
	case "reasoning":
		return detectProviderByEnv(cat, []string{"REASONING_PROVIDER"}, []string{"REASONING_BASE_URL"}, []string{"REASONING_MODEL"})
	case "agent":
		return detectProviderByEnv(cat, []string{"TACHI_AGENT_BACKEND"}, nil, []string{"TACHI_AGENT_MODEL"})
	default:
		return cat.selected
	}
}

func detectProviderByEnv(cat providerCategory, providerKeys, endpointKeys, modelKeys []string) int {
	for _, key := range providerKeys {
		if provider, ok := readEnvKey(key); ok {
			provider = strings.TrimSpace(provider)
			for i, opt := range cat.options {
				if opt.id == provider || strings.EqualFold(opt.name, provider) {
					return i
				}
			}
		}
	}
	for _, key := range endpointKeys {
		if endpoint, ok := readEnvKey(key); ok {
			for i, opt := range cat.options {
				if opt.endpoint != "" && opt.endpoint == endpoint {
					return i
				}
			}
		}
		_ = key
	}
	for _, key := range modelKeys {
		if model, ok := readEnvKey(key); ok {
			for i, opt := range cat.options {
				if opt.model != "" && opt.model == model {
					return i
				}
			}
		}
	}
	return cat.selected
}

func (s *providersStep) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case providersWriteDoneMsg:
		s.phase = providersPhaseDone
		s.writeErrMsg = msg.err
		return s, nil
	case tea.KeyMsg:
		switch s.phase {
		case providersPhaseDone, providersPhaseSkip:
			if msg.String() == "enter" {
				return s, stepDone()
			}
			return s, nil
		case providersPhaseCustomEndpoint:
			return s.handleCustomEndpointKey(msg)
		case providersPhaseSelect:
			switch msg.String() {
			case "up", "k":
				if s.cursor > 0 {
					s.cursor--
				}
			case "down", "j":
				if s.cursor < len(s.categories)-1 {
					s.cursor++
				}
			case "left", "h":
				cat := &s.categories[s.cursor]
				if cat.selected > 0 {
					cat.selected--
				}
			case "right", "l":
				cat := &s.categories[s.cursor]
				if cat.selected < len(cat.options)-1 {
					cat.selected++
				}
			case "enter":
				s.state.ProviderSelections = s.buildSelections()
				s.pending = s.customEndpointSelections()
				if len(s.pending) > 0 {
					s.pendingIdx = 0
					s.prepareCustomInput()
					s.phase = providersPhaseCustomEndpoint
					return s, textinput.Blink
				}
				s.phase = providersPhaseWriting
				return s, s.writeConfigEnv()
			case "s":
				s.phase = providersPhaseSkip
				return s, nil
			}
		}
	}

	if s.phase == providersPhaseCustomEndpoint {
		var cmd tea.Cmd
		s.input, cmd = s.input.Update(msg)
		return s, cmd
	}
	return s, nil
}

func (s *providersStep) handleCustomEndpointKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	switch msg.String() {
	case "enter":
		endpoint := strings.TrimSpace(s.input.Value())
		if endpoint == "" || !looksLikeURL(endpoint) {
			s.input.Placeholder = T("Enter a full https:// URL", "输入完整 https:// URL")
			return s, nil
		}
		catIdx, optIdx := s.pending[s.pendingIdx][0], s.pending[s.pendingIdx][1]
		s.customURLs[s.customURLKey(catIdx, optIdx)] = endpoint
		selection := s.selectionFor(s.categories[catIdx], s.categories[catIdx].options[optIdx])
		selection.Endpoint = endpoint
		s.state.ProviderSelections[s.categories[catIdx].id] = selection
		s.pendingIdx++
		if s.pendingIdx < len(s.pending) {
			s.prepareCustomInput()
			return s, textinput.Blink
		}
		s.input.Blur()
		s.phase = providersPhaseWriting
		return s, s.writeConfigEnv()
	case "esc":
		s.phase = providersPhaseSelect
		s.input.Blur()
		return s, nil
	}
	var cmd tea.Cmd
	s.input, cmd = s.input.Update(msg)
	return s, cmd
}

func looksLikeURL(raw string) bool {
	parsed, err := url.Parse(raw)
	return err == nil && parsed.Scheme != "" && parsed.Host != ""
}

func (s *providersStep) prepareCustomInput() {
	catIdx, optIdx := s.pending[s.pendingIdx][0], s.pending[s.pendingIdx][1]
	opt := s.categories[catIdx].options[optIdx]
	current := s.customURLs[s.customURLKey(catIdx, optIdx)]
	if current == "" {
		current = opt.endpoint
	}
	s.input.SetValue(current)
	s.input.Placeholder = T("https://api.example.com/v1/...", "https://api.example.com/v1/...")
	s.input.Focus()
}

func (s *providersStep) customEndpointSelections() [][2]int {
	var pending [][2]int
	for catIdx, cat := range s.categories {
		opt := cat.options[cat.selected]
		if opt.requiresURL {
			pending = append(pending, [2]int{catIdx, cat.selected})
		}
	}
	return pending
}

func (s *providersStep) customURLKey(catIdx, optIdx int) string {
	return fmt.Sprintf("%d:%d", catIdx, optIdx)
}

func (s *providersStep) buildSelections() map[string]ProviderSelection {
	selections := make(map[string]ProviderSelection)
	for _, cat := range s.categories {
		opt := cat.options[cat.selected]
		selections[cat.id] = s.selectionFor(cat, opt)
	}
	return selections
}

func (s *providersStep) selectionFor(cat providerCategory, opt providerOption) ProviderSelection {
	return ProviderSelection{
		CategoryID: cat.id,
		Name:       opt.name,
		ProviderID: opt.id,
		APIKeyEnv:  opt.apiKeyEnv,
		Endpoint:   opt.endpoint,
		Model:      opt.model,
	}
}

func (s *providersStep) View() string {
	var sb strings.Builder
	sb.WriteString("\n")

	switch s.phase {
	case providersPhaseCustomEndpoint:
		catIdx, optIdx := s.pending[s.pendingIdx][0], s.pending[s.pendingIdx][1]
		cat := s.categories[catIdx]
		opt := cat.options[optIdx]
		sb.WriteString("  " + T("Custom endpoint for", "自定义端点:") + " ")
		sb.WriteString(valueStyle.Render(cat.label+" → "+opt.name) + "\n\n")
		sb.WriteString(s.input.View() + "\n")
		sb.WriteString(hintStyle.Render(T("\n  enter: save endpoint  ·  esc: back", "\n  回车: 保存端点  ·  esc: 返回")))
		return sb.String()
	case providersPhaseWriting:
		sb.WriteString("  " + lipgloss.NewStyle().Foreground(accent).Render("...") + T(" Writing provider settings to ~/.tachi/config.env...", " 正在写入供应商设置到 ~/.tachi/config.env...") + "\n")
		return sb.String()
	case providersPhaseSkip:
		sb.WriteString(warnStyle.Render("  ! "+T("Skipped", "已跳过")) + T(" — provider defaults unchanged\n", " — 供应商默认值未修改\n"))
		sb.WriteString("\n" + hintStyle.Render(T("  Press Enter to continue", "  按回车继续")))
		return sb.String()
	case providersPhaseDone:
		if s.writeErrMsg != "" {
			sb.WriteString("  " + crossStyle.Render("x "+T("Provider config failed: ", "供应商配置失败: ")+s.writeErrMsg) + "\n")
		} else {
			sb.WriteString("  " + checkStyle.Render("✓ "+T("Provider settings saved", "供应商设置已保存")) + "\n")
		}
		for _, cat := range s.categories {
			opt := cat.options[cat.selected]
			sb.WriteString(fmt.Sprintf("  %-22s %s\n", labelDimStyle.Render(cat.label+":"), valueStyle.Render(opt.name)))
		}
		missing := requiredProviderKeys(s.state)
		if len(missing) > 0 {
			sb.WriteString("\n  " + warnStyle.Render(T("Missing selected provider keys:", "缺少所选供应商密钥:")) + "\n")
			for _, key := range missing {
				sb.WriteString("    " + key + "\n")
			}
		}
		sb.WriteString("\n" + hintStyle.Render(T("  Press Enter to continue", "  按回车继续")))
		return sb.String()
	}

	for i, cat := range s.categories {
		isCurrent := i == s.cursor
		opt := cat.options[cat.selected]
		prefix := "  "
		labelStyle := lipgloss.NewStyle().Foreground(textBright)
		if isCurrent {
			prefix = lipgloss.NewStyle().Foreground(accent).Bold(true).Render("▐ ")
			labelStyle = labelStyle.Bold(true)
		}
		sb.WriteString(prefix + labelStyle.Render(cat.label) + "\n")
		sb.WriteString(s.renderProviderOption(cat, opt, isCurrent) + "\n")
		if isCurrent {
			sb.WriteString("      " + lipgloss.NewStyle().Foreground(textDim).Italic(true).Render(opt.desc) + "\n")
			if opt.endpoint != "" {
				sb.WriteString("      " + labelDimStyle.Render("endpoint: ") + shortEndpoint(opt.endpoint) + "\n")
			}
			if opt.apiKeyEnv != "" {
				sb.WriteString("      " + providerKeyStatus(opt.apiKeyEnv) + "\n")
			}
		}
		sb.WriteString("\n")
	}

	sb.WriteString(hintStyle.Render(T(
		"  ↑/↓: category  ·  ←/→: provider  ·  enter: save  ·  s: skip",
		"  ↑/↓: 分类  ·  ←/→: 供应商  ·  回车: 保存  ·  s: 跳过",
	)))
	return sb.String()
}

func (s *providersStep) renderProviderOption(cat providerCategory, opt providerOption, active bool) string {
	leftArrow := " "
	rightArrow := " "
	if active {
		if cat.selected > 0 {
			leftArrow = lipgloss.NewStyle().Foreground(textDim).Render("<")
		}
		if cat.selected < len(cat.options)-1 {
			rightArrow = lipgloss.NewStyle().Foreground(textDim).Render(">")
		}
	}
	name := opt.name
	var tags []string
	if opt.recommended {
		tags = append(tags, T("recommended", "推荐"))
	}
	if opt.isDefault {
		tags = append(tags, T("default", "默认"))
	}
	if len(tags) > 0 {
		name += " " + lipgloss.NewStyle().Foreground(accent).Render("("+strings.Join(tags, ", ")+")")
	}
	style := lipgloss.NewStyle().Foreground(textDim)
	if active {
		style = lipgloss.NewStyle().Foreground(textBright).Bold(true)
	}
	return fmt.Sprintf("      %s %s %s", leftArrow, style.Render(name), rightArrow)
}

func providerKeyStatus(apiKeyEnv string) string {
	if _, ok := readEnvKey(apiKeyEnv); ok {
		return checkStyle.Render("✓ " + apiKeyEnv)
	}
	return warnStyle.Render("! " + apiKeyEnv + T(" (not configured)", "（未配置）"))
}

func shortEndpoint(endpoint string) string {
	if len(endpoint) <= 64 {
		return endpoint
	}
	return endpoint[:61] + "..."
}

type providersWriteDoneMsg struct{ err string }

func (s *providersStep) writeConfigEnv() tea.Cmd {
	entries := providerEnvEntries(s.state.ProviderSelections)
	return func() tea.Msg {
		if err := upsertConfigEnvKeys(entries); err != nil {
			return providersWriteDoneMsg{err: err.Error()}
		}
		return providersWriteDoneMsg{}
	}
}

func providerEnvEntries(selections map[string]ProviderSelection) map[string]string {
	entries := make(map[string]string)
	if selections == nil {
		return entries
	}

	if emb, ok := selections["embedding"]; ok {
		entries["EMBEDDING_PROVIDER"] = emb.ProviderID
		if emb.Endpoint != "" {
			entries["EMBEDDING_BASE_URL"] = emb.Endpoint
		}
		if emb.Model != "" {
			entries["EMBEDDING_MODEL"] = emb.Model
		}
	}

	if reasoning, ok := selections["reasoning"]; ok {
		entries["REASONING_PROVIDER"] = reasoning.ProviderID
		if reasoning.Endpoint != "" {
			entries["REASONING_BASE_URL"] = reasoning.Endpoint
		}
		if reasoning.Model != "" {
			entries["REASONING_MODEL"] = reasoning.Model
		}
		if reasoning.ProviderID == "siliconflow" {
			entries["SILICONFLOW_BASE_URL"] = reasoning.Endpoint
			entries["SILICONFLOW_MODEL"] = reasoning.Model
		}
	}

	if agent, ok := selections["agent"]; ok {
		backend := agent.ProviderID
		switch backend {
		case "claude-code", "glm-5.1-claude":
			backend = "claude"
		}
		entries["TACHI_AGENT_BACKEND"] = backend
		if agent.Model != "" && agent.ProviderID != "custom" {
			entries["TACHI_AGENT_MODEL"] = agent.Model
		}
		if agent.Endpoint != "" {
			entries["TACHI_AGENT_BASE_URL"] = agent.Endpoint
		}
	}

	return entries
}

func requiredProviderKeys(state *State) []string {
	seen := make(map[string]bool)
	var keys []string
	for _, selection := range state.ProviderSelections {
		key := strings.TrimSpace(selection.APIKeyEnv)
		if key == "" || seen[key] {
			continue
		}
		seen[key] = true
		if _, ok := readEnvKey(key); !ok {
			keys = append(keys, key)
		}
	}
	return keys
}

func providerRequiredKeySet(state *State) map[string]bool {
	keys := make(map[string]bool)
	for _, selection := range state.ProviderSelections {
		if selection.APIKeyEnv != "" {
			keys[selection.APIKeyEnv] = true
		}
	}
	return keys
}

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
	if len(entries) == 0 {
		return nil
	}
	home, err := homeDir()
	if err != nil {
		return err
	}
	configPath := filepath.Join(home, ".tachi", "config.env")
	if err := os.MkdirAll(filepath.Dir(configPath), 0755); err != nil {
		return err
	}

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
			// Don't overwrite real API keys with $REF placeholders.
			if strings.HasPrefix(val, "$") {
				updated[key] = true
				continue
			}
			lines[i] = key + "=" + val
			updated[key] = true
		}
	}

	needsHeader := true
	for key, val := range entries {
		if updated[key] || strings.HasPrefix(val, "$") {
			continue
		}
		if needsHeader {
			lines = append(lines, "", "# Tachi provider configuration")
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
