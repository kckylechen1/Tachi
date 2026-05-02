package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io"
	"os/exec"
	"sync"
)

// MCPClient communicates with tachi via MCP stdio protocol (JSON-RPC).
type MCPClient struct {
	cmd    *exec.Cmd
	stdin  io.WriteCloser
	stdout *bufio.Reader
	mu     sync.Mutex
	nextID int
}

type jsonRPCRequest struct {
	JSONRPC string      `json:"jsonrpc"`
	ID      int         `json:"id"`
	Method  string      `json:"method"`
	Params  interface{} `json:"params,omitempty"`
}

type jsonRPCResponse struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      int             `json:"jsonrpc"`
	Result  json.RawMessage `json:"result,omitempty"`
	Error   *jsonRPCError   `json:"error,omitempty"`
}

type jsonRPCError struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
}

func NewMCPClient() (*MCPClient, error) {
	cmd := exec.Command("tachi")
	stdin, err := cmd.StdinPipe()
	if err != nil {
		return nil, fmt.Errorf("stdin pipe: %w", err)
	}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return nil, fmt.Errorf("stdout pipe: %w", err)
	}
	cmd.Stderr = nil // discard tachi logs

	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("start tachi: %w", err)
	}

	tc := &MCPClient{
		cmd:    cmd,
		stdin:  stdin,
		stdout: bufio.NewReader(stdout),
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
	notif := jsonRPCRequest{
		JSONRPC: "2.0",
		Method:  "notifications/initialized",
	}
	data, _ := json.Marshal(notif)
	data = append(data, '\n')
	tc.stdin.Write(data)

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
	tc.stdin.Close()
	tc.cmd.Wait()
}
