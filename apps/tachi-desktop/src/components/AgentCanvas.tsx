import { useEffect, useRef, useState } from 'react';
import ForceGraph2D from 'react-force-graph-2d';
import {
  DEFAULT_TACHI_DAEMON_HOST,
  getApiErrorMessage,
  tachiApi,
  type MemoryEntry,
} from '../services/api';
import type { InspectableItem } from './Inspector';

interface GraphNode extends InspectableItem {
  group: 1 | 2 | 3;
}

interface GraphLink {
  source: string;
  target: string;
  value: number;
}

interface GraphDataShape {
  nodes: GraphNode[];
  links: GraphLink[];
}

interface AgentCanvasProps {
  onNodeClick: (item: InspectableItem) => void;
}

const BASE_GRAPH: GraphDataShape = {
  nodes: [
    {
      id: 'tachi',
      group: 1,
      kind: 'core',
      label: 'Tachi Hub',
      details: { status: 'idle' },
    },
  ],
  links: [],
};

function metadataFromCard(card: MemoryEntry): Record<string, unknown> {
  return typeof card.metadata === 'object' && card.metadata !== null ? card.metadata : {};
}

function stringField(value: unknown, fallback: string): string {
  return typeof value === 'string' && value.trim() ? value : fallback;
}

function buildKanbanGraph(cards: MemoryEntry[]): GraphDataShape {
  const baseNode: GraphNode = BASE_GRAPH.nodes[0];
  const nodes = new Map<string, GraphNode>([[baseNode.id, baseNode]]);
  const links = new Map<string, GraphLink>();
  const stats = new Map<string, { inbound: number; outbound: number; openCards: number; highPriority: number }>();

  const getOrCreateStats = (agentId: string) => {
    const current = stats.get(agentId);
    if (current) {
      return current;
    }

    const created = { inbound: 0, outbound: 0, openCards: 0, highPriority: 0 };
    stats.set(agentId, created);
    return created;
  };

  for (const card of cards) {
    const metadata = metadataFromCard(card);
    const fromAgent = stringField(metadata.from_agent, 'agent:unknown-source');
    const toAgent = stringField(metadata.to_agent, 'agent:unknown-target');
    const status = stringField(metadata.status, 'open');
    const priority = stringField(metadata.priority, 'medium');

    const fromStats = getOrCreateStats(fromAgent);
    fromStats.outbound += 1;
    if (status === 'open') {
      fromStats.openCards += 1;
    }
    if (priority === 'high' || priority === 'critical') {
      fromStats.highPriority += 1;
    }

    const toStats = getOrCreateStats(toAgent);
    toStats.inbound += 1;

    if (!nodes.has(fromAgent)) {
      nodes.set(fromAgent, {
        id: fromAgent,
        label: fromAgent.replace(/^agent:/, ''),
        kind: 'agent',
        group: 2,
        details: {},
      });
    }
    if (!nodes.has(toAgent)) {
      nodes.set(toAgent, {
        id: toAgent,
        label: toAgent.replace(/^agent:/, ''),
        kind: 'agent',
        group: 2,
        details: {},
      });
    }

    const linkKey = `${fromAgent}-->${toAgent}`;
    const currentLink = links.get(linkKey);
    if (currentLink) {
      currentLink.value += 1;
    } else {
      links.set(linkKey, { source: fromAgent, target: toAgent, value: 1 });
    }
  }

  for (const [agentId, agentStats] of stats.entries()) {
    const node = nodes.get(agentId);
    if (!node) {
      continue;
    }
    node.details = {
      inbound: agentStats.inbound,
      outbound: agentStats.outbound,
      openCards: agentStats.openCards,
      highPriority: agentStats.highPriority,
    };
  }

  return {
    nodes: [...nodes.values()],
    links: [...links.values()],
  };
}

export function AgentCanvas({ onNodeClick }: AgentCanvasProps) {
  const [dimensions, setDimensions] = useState({ width: 800, height: 600 });
  const containerRef = useRef<HTMLDivElement>(null);
  const [graphData, setGraphData] = useState<GraphDataShape>(BASE_GRAPH);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    // Resize observer for responsive canvas
    if (!containerRef.current) return;
    const observer = new ResizeObserver(entries => {
      for (const entry of entries) {
        setDimensions({ width: entry.contentRect.width, height: entry.contentRect.height });
      }
    });
    observer.observe(containerRef.current);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    let cancelled = false;

    const poll = async () => {
      try {
        const nextGraph = buildKanbanGraph(await tachiApi.fetchKanbanCards(120));

        if (!cancelled) {
          setGraphData(nextGraph.nodes.length > 0 ? nextGraph : BASE_GRAPH);
          setError(null);
        }
      } catch (fetchError) {
        if (!cancelled) {
          setError(getApiErrorMessage(fetchError));
          setGraphData(BASE_GRAPH);
        }
      } finally {
        if (!cancelled) {
          setIsLoading(false);
        }
      }
    };

    void poll();
    const intervalId = window.setInterval(() => {
      void poll();
    }, 4000);

    return () => {
      cancelled = true;
      window.clearInterval(intervalId);
    };
  }, []);

  const isEmptyGraph = graphData.nodes.length <= 1 && graphData.links.length === 0;

  return (
    <div ref={containerRef} style={{ width: '100%', height: '100%', position: 'absolute' }}>
      <ForceGraph2D
        width={dimensions.width}
        height={dimensions.height}
        graphData={graphData}
        nodeLabel="label"
        nodeColor={(node) => {
          if (node.group === 1) return '#ffffff'; // Core Hub
          if (node.group === 2) return '#0df'; // Agents (Neon Cyan)
          return '#f05'; // Pub/Sub Topics (Neon Magenta)
        }}
        linkColor={() => 'rgba(0, 221, 255, 0.3)'}
        nodeRelSize={6}
        linkDirectionalParticles={2}
        linkDirectionalParticleSpeed={(link) => link.value * 0.005}
        onNodeClick={(node) => onNodeClick(node)}
        backgroundColor="var(--bg-color)"
      />

      {(isLoading || error || isEmptyGraph) && (
        <div
          style={{
            position: 'absolute',
            inset: 0,
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            pointerEvents: 'none',
            background: 'linear-gradient(180deg, rgba(13,13,18,0.1), rgba(13,13,18,0.25))',
          }}
        >
          <div className="kanban-card" style={{ maxWidth: 380, textAlign: 'center' }}>
            <div style={{ marginBottom: 6 }}>
              {isLoading ? 'Syncing live daemon graph...' : error ? 'Daemon Offline' : 'No live events yet'}
            </div>
            <div className="text-muted" style={{ fontSize: '0.82rem' }}>
              {error
                ? `Unable to fetch live MCP data from ${DEFAULT_TACHI_DAEMON_HOST}.`
                : 'Waiting for Kanban activity to appear.'}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
