import { LayoutDashboard, Brain, HardDrive, Settings } from 'lucide-react';

interface SidebarProps {
  activeTab: string;
  setActiveTab: (tab: string) => void;
  daemonOnline: boolean;
}

export function Sidebar({ activeTab, setActiveTab, daemonOnline }: SidebarProps) {
  const navItems = [
    { id: 'kanban', label: 'Kanban Flow', icon: LayoutDashboard },
    { id: 'memory', label: 'Memory Explorer', icon: Brain },
    { id: 'hub', label: 'Hub Capabilities', icon: HardDrive },
    { id: 'settings', label: 'Settings', icon: Settings },
  ];

  return (
    <aside className="glass-panel sidebar" style={{ display: 'flex', flexDirection: 'column' }}>
      <div style={{ padding: '0 8px 32px 8px' }}>
        <h1 style={{ fontSize: '1.5rem', margin: 0 }}>Tachi<span style={{ color: 'var(--accent-cyan)' }}>OS</span></h1>
        <div className="text-muted" style={{ fontSize: '0.8rem' }}>Agent Hub & Execution</div>
      </div>
      
      <nav style={{ flex: 1 }}>
        {navItems.map(item => (
          <div 
            key={item.id}
            className={`nav-item ${activeTab === item.id ? 'active' : ''}`}
            onClick={() => setActiveTab(item.id)}
          >
            <item.icon size={18} />
            <span style={{ fontWeight: 500 }}>{item.label}</span>
          </div>
        ))}
      </nav>

      <div className="kanban-card text-muted" style={{ fontSize: '0.75rem', marginTop: 'auto' }}>
        <div style={{ display: 'flex', justifyContent: 'space-between', marginBottom: 4 }}>
          <span>Daemon Status</span>
          <span style={{ color: daemonOnline ? 'var(--accent-cyan)' : 'var(--accent-magenta)' }}>
            {daemonOnline ? 'Online' : 'Offline'}
          </span>
        </div>
        <div style={{ display: 'flex', justifyContent: 'space-between' }}>
          <span>Memory Store</span>
          <span>{daemonOnline ? 'SQLite WAL' : '--'}</span>
        </div>
      </div>
    </aside>
  );
}
