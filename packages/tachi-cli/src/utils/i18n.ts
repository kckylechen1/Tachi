import { TACHI_CLI_VERSION } from '../version.js';

export type Language = 'en' | 'zh';

interface Translations {
  [key: string]: string | Translations;
}

const en: Translations = {
  app: {
    name: 'Tachi',
    tagline: 'Memory \u0026 Capability Orchestrator',
    version: `v${TACHI_CLI_VERSION}`,
  },
  menu: {
    title: 'Main Menu',
    daemon: 'Daemon Management',
    mcp: 'MCP Servers',
    skills: 'Skills',
    memory: 'Memory Browser',
    settings: 'Settings',
    exit: 'Exit',
    select: 'Select action',
    hint: '↑↓ Navigate | Enter Confirm | q Quit',
  },
  daemon: {
    title: 'Daemon Management',
    status: 'Daemon Status',
    online: 'Online',
    offline: 'Offline',
    starting: 'Starting',
    port: 'Port',
    memory: 'Memory Entries',
    start: 'Start Daemon',
    stop: 'Stop Daemon',
    restart: 'Restart Daemon',
    logs: 'View Logs',
    back: '← Back',
  },
  mcp: {
    title: 'MCP Servers',
    configured: 'Configured Servers',
    add: 'Add Server',
    remove: 'Remove Server',
    enable: 'Enable',
    disable: 'Disable',
    enabled: 'enabled',
    disabled: 'disabled',
  },
  skills: {
    title: 'Skills Management',
    browse: 'Browse Skills',
    import: 'Import Skill',
    export: 'Export Skills',
    evolve: 'Evolve Skill',
    active: 'Active Skills',
  },
  memory: {
    title: 'Memory Browser',
    search: 'Search Memory',
    ghost: 'Ghost Channels',
    kanban: 'Kanban Board',
  },
  settings: {
    title: 'Settings',
    language: 'Language',
    dataDir: 'Data Directory',
    autoStart: 'Auto-start on Login',
    theme: 'Theme',
    apiKeys: 'API Keys (Vault)',
  },
  init: {
    welcome: 'Welcome to Tachi!',
    setup: 'Let\'s set up your memory orchestrator',
    dataDir: 'Choose data directory',
    defaultDir: 'Default: ~/.tachi',
    complete: 'Setup complete!',
    startNow: 'Start daemon now?',
    keysTitle: 'Provider API keys (optional)',
    keysIntro: 'Store API keys in the encrypted vault. Press Enter to skip any key.',
    vaultPassword: 'Vault master password',
    vaultPasswordHint: 'Required to encrypt keys. Run `tachi vault init` first if unset.',
    keyEnter: 'Enter value (masked)',
    keySkip: 'Enter Skip',
    keySaving: 'Saving to vault...',
    keySaved: 'Saved',
    keyFailed: 'Could not save',
    keysSkipAll: 'Esc Skip all remaining',
  },
  common: {
    yes: 'Yes',
    no: 'No',
    back: '← Back',
    cancel: 'Cancel',
    confirm: 'Confirm',
    loading: 'Loading...',
    error: 'Error',
    success: 'Success',
  },
};

const zh: Translations = {
  app: {
    name: 'Tachi',
    tagline: '记忆与能力编排器',
    version: `v${TACHI_CLI_VERSION}`,
  },
  menu: {
    title: '主菜单',
    daemon: '守护进程管理',
    mcp: 'MCP 服务器',
    skills: '技能管理',
    memory: '内存浏览器',
    settings: '设置',
    exit: '退出',
    select: '选择操作',
    hint: '↑↓ 导航 | Enter 确认 | q 退出',
  },
  daemon: {
    title: '守护进程管理',
    status: '守护进程状态',
    online: '运行中',
    offline: '已停止',
    starting: '启动中',
    port: '端口',
    memory: '记忆条目',
    start: '启动守护进程',
    stop: '停止守护进程',
    restart: '重启守护进程',
    logs: '查看日志',
    back: '← 返回',
  },
  mcp: {
    title: 'MCP 服务器',
    configured: '已配置的服务器',
    add: '添加服务器',
    remove: '移除服务器',
    enable: '启用',
    disable: '禁用',
    enabled: '已启用',
    disabled: '已禁用',
  },
  skills: {
    title: '技能管理',
    browse: '浏览技能',
    import: '导入技能',
    export: '导出技能',
    evolve: '进化技能',
    active: '活跃技能',
  },
  memory: {
    title: '内存浏览器',
    search: '搜索记忆',
    ghost: 'Ghost 频道',
    kanban: '看板',
  },
  settings: {
    title: '设置',
    language: '语言',
    dataDir: '数据目录',
    autoStart: '登录时自动启动',
    theme: '主题',
    apiKeys: 'API 密钥（保险库）',
  },
  init: {
    welcome: '欢迎使用 Tachi！',
    setup: '让我们设置您的记忆编排器',
    dataDir: '选择数据目录',
    defaultDir: '默认: ~/.tachi',
    complete: '设置完成！',
    startNow: '立即启动守护进程？',
    keysTitle: '提供商 API 密钥（可选）',
    keysIntro: '将 API 密钥存入加密保险库。按 Enter 可跳过任意密钥。',
    vaultPassword: '保险库主密码',
    vaultPasswordHint: '用于加密密钥。若尚未设置，请先运行 `tachi vault init`。',
    keyEnter: '输入值（已遮蔽）',
    keySkip: 'Enter 跳过',
    keySaving: '正在保存到保险库...',
    keySaved: '已保存',
    keyFailed: '无法保存',
    keysSkipAll: 'Esc 跳过其余全部',
  },
  common: {
    yes: '是',
    no: '否',
    back: '← 返回',
    cancel: '取消',
    confirm: '确认',
    loading: '加载中...',
    error: '错误',
    success: '成功',
  },
};

let currentLang: Language = 'en';

export function setLanguage(lang: Language) {
  currentLang = lang;
}

export function getLanguage(): Language {
  return currentLang;
}

function lookup(dict: Translations, path: string): string | undefined {
  let current: Translations | string = dict;
  for (const key of path.split('.')) {
    if (typeof current === 'object' && current !== null && key in current) {
      current = (current as Translations)[key];
    } else {
      return undefined;
    }
  }
  return typeof current === 'string' ? current : undefined;
}

export function t(path: string): string {
  // Current language first, then fall back to English so a key missing only
  // from the zh dictionary degrades to readable English rather than leaking the
  // raw dotted key. As a last resort show the leaf segment, never the full path.
  const primary = currentLang === 'zh' ? zh : en;
  return (
    lookup(primary, path) ??
    // Only retry against English when it isn't already the primary dictionary.
    (primary === en ? undefined : lookup(en, path)) ??
    path.split('.').pop() ??
    path
  );
}