package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"sync"
)

// MCPClient communicates with tachi via MCP stdio protocol (JSON-RPC).
type MCPClient struct {
	cmd     *exec.Cmd
	stdin   *bufio.Writer
	stdout  *bufio.Reader
	closeFn func()
	mu      sync.Mutex
	nextID  int
}

type jsonRPCRequest struct {
	JSONRPC string      `json:"jsonrpc"`
	ID      int         `json:"id"`
	Method  string      `json:"method"`
	Params  interface{} `json:"params,omitempty"`
}

type jsonRPCResponse struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      int             `json:"id"`
	Result  json.RawMessage `json:"result,omitempty"`
	Error   *jsonRPCError   `json:"error,omitempty"`
}

type jsonRPCError struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
}

func NewMCPClient() (*MCPClient, error) {
	cmd := exec.Command("tachi")
	// Force admin profile so all tools (including vault) are accessible,
	// regardless of any TACHI_EXPOSED_TOOLS or TACHI_PROFILE in the user's env.
	cmd.Env = append(os.Environ(),
		"TACHI_PROFILE=admin",
		"TACHI_EXPOSED_TOOLS=",
	)
	stdinPipe, err := cmd.StdinPipe()
	if err != nil {
		return nil, fmt.Errorf("stdin pipe: %w", err)
	}
	stdoutPipe, err := cmd.StdoutPipe()
	if err != nil {
		return nil, fmt.Errorf("stdout pipe: %w", err)
	}
	cmd.Stderr = os.Stderr

	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("start tachi: %w", err)
	}

	// Wrap stdin in a buffered writer so we can flush after each request
	stdin := bufio.NewWriter(stdinPipe)

	tc := &MCPClient{
		cmd:    cmd,
		stdin:  stdin,
		stdout: bufio.NewReader(stdoutPipe),
		closeFn: func() {
			stdinPipe.Close()
			cmd.Wait()
		},
	}

	if err := tc.initialize(); err != nil {
		tc.Close()
		return nil, err
	}

	return tc, nil
}

func (tc *MCPClient) initialize() error {
	_, err := tc.sendRequest("initialize", map[string]interface{}{
		"protocolVersion": "2024-11-05",
		"capabilities":    map[string]interface{}{},
		"clientInfo": map[string]string{
			"name":    "tachi-helper",
			"version": "0.1.0",
		},
	})
	if err != nil {
		return fmt.Errorf("MCP initialize: %w", err)
	}

	// Send initialized notification (no ID = notification)
	notif := map[string]string{
		"jsonrpc": "2.0",
		"method":  "notifications/initialized",
	}
	data, _ := json.Marshal(notif)
	data = append(data, '\n')
	tc.stdin.Write(data)
	tc.stdin.Flush()

	return nil
}

func (tc *MCPClient) CallTool(name string, args map[string]interface{}) (json.RawMessage, error) {
	params := map[string]interface{}{
		"name":      name,
		"arguments": args,
	}
	return tc.sendRequest("tools/call", params)
}

func (tc *MCPClient) sendRequest(method string, params interface{}) (json.RawMessage, error) {
	tc.mu.Lock()
	defer tc.mu.Unlock()

	tc.nextID++
	req := jsonRPCRequest{
		JSONRPC: "2.0",
		ID:      tc.nextID,
		Method:  method,
		Params:  params,
	}

	data, err := json.Marshal(req)
	if err != nil {
		return nil, fmt.Errorf("marshal request: %w", err)
	}
	data = append(data, '\n')

	if _, err := tc.stdin.Write(data); err != nil {
		return nil, fmt.Errorf("write request: %w", err)
	}
	if err := tc.stdin.Flush(); err != nil {
		return nil, fmt.Errorf("flush request: %w", err)
	}

	// Read response lines until we get one with matching ID
	for {
		line, err := tc.stdout.ReadString('\n')
		if err != nil {
			return nil, fmt.Errorf("read response: %w", err)
		}
		if line == "\n" || line == "" {
			continue
		}

		var resp jsonRPCResponse
		if err := json.Unmarshal([]byte(line), &resp); err != nil {
			continue // skip non-JSON lines (server logs)
		}
		if resp.ID != req.ID {
			continue // notification or different response
		}
		if resp.Error != nil {
			return nil, fmt.Errorf("RPC error: %s", resp.Error.Message)
		}
		return resp.Result, nil
	}
}

func (tc *MCPClient) Close() {
	tc.closeFn()
}
