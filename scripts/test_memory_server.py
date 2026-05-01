#!/usr/bin/env python3
"""
测试 Memory MCP Server — 双 DB (global + project) 功能
"""

import subprocess
import json
import sys
import os
import tempfile
import shutil

# 用临时目录隔离测试
tmpdir = tempfile.mkdtemp(prefix="sigil_test_")
global_db = os.path.join(tmpdir, "global", "memory.db")
project_dir = os.path.join(tmpdir, "project")
os.makedirs(project_dir)
# 创建 fake git repo 让 find_git_root() 生效
os.makedirs(os.path.join(project_dir, ".git"))
os.makedirs(os.path.join(project_dir, ".sigil"), exist_ok=True)

env = os.environ.copy()
env.update({
    "VOYAGE_API_KEY": os.environ.get("VOYAGE_API_KEY", ""),
    "SILICONFLOW_API_KEY": os.environ.get("SILICONFLOW_API_KEY", ""),
    "MEMORY_DB_PATH": global_db,
    "ENABLE_PIPELINE": "false",
})

REQUEST_ID = 0

def next_id():
    global REQUEST_ID
    REQUEST_ID += 1
    return REQUEST_ID

def send_request(proc, request):
    request_str = json.dumps(request) + "\n"
    proc.stdin.write(request_str.encode())
    proc.stdin.flush()
    response_line = proc.stdout.readline().decode().strip()
    if response_line:
        return json.loads(response_line)
    return None

def call_tool(proc, name, arguments=None):
    req = {
        "jsonrpc": "2.0",
        "id": next_id(),
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments or {}},
    }
    return send_request(proc, req)

def extract_text(response):
    """Extract text content from MCP tool response."""
    try:
        content = response["result"]["content"]
        for item in content:
            if item.get("type") == "text":
                return json.loads(item["text"])
    except (KeyError, TypeError, json.JSONDecodeError):
        pass
    return response

def test_memory_server():
    print("Starting Memory Server...")
    proc = subprocess.Popen(
        ["/Users/kckylechen/Desktop/Sigil/target/release/memory-server"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        cwd=project_dir,  # run inside fake git repo
    )

    passed = 0
    failed = 0

    def check(name, condition, detail=""):
        nonlocal passed, failed
        if condition:
            passed += 1
            print(f"  PASS  {name}")
        else:
            failed += 1
            print(f"  FAIL  {name}  {detail}")

    try:
        # Initialize
        resp = send_request(proc, {
            "jsonrpc": "2.0", "id": next_id(), "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "test-client", "version": "1.0.0"},
            },
        })
        check("initialize", resp and "result" in resp)

        # initialized notification
        proc.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}).encode() + b"\n")
        proc.stdin.flush()

        # ── Test 1: save to global ──
        print("\n[Test 1] save_memory scope=global")
        resp = call_tool(proc, "save_memory", {
            "text": "User prefers bun over npm",
            "scope": "global",
            "category": "preference",
            "importance": 0.9,
            "path": "/user/prefs",
        })
        data = extract_text(resp)
        check("save global returns db=global", data.get("db") == "global", f"got: {data}")
        global_id = data.get("id", "")

        # ── Test 2: save to project (default) ──
        print("\n[Test 2] save_memory scope=project")
        resp = call_tool(proc, "save_memory", {
            "text": "Sigil uses rmcp for MCP transport",
            "scope": "project",
            "category": "fact",
            "importance": 0.8,
            "path": "/sigil/arch",
        })
        data = extract_text(resp)
        check("save project returns db=project", data.get("db") == "project", f"got: {data}")
        project_id = data.get("id", "")

        # ── Test 3: search returns results from both DBs ──
        print("\n[Test 3] search_memory (dual-DB)")
        resp = call_tool(proc, "search_memory", {
            "query": "bun OR rmcp OR test",
            "top_k": 10,
        })
        results = extract_text(resp)
        check("search returns list", isinstance(results, list), f"type: {type(results)}, val: {str(results)[:200]}")
        if isinstance(results, list) and len(results) > 0:
            dbs_seen = set(r.get("db") for r in results)
            check("search has global results", "global" in dbs_seen, f"dbs: {dbs_seen}")
            check("search has project results", "project" in dbs_seen, f"dbs: {dbs_seen}")
            check("results have relevance score", all("relevance" in r for r in results))

        # ── Test 4: get_memory from global ──
        print("\n[Test 4] get_memory (global entry)")
        resp = call_tool(proc, "get_memory", {"id": global_id})
        data = extract_text(resp)
        check("get global entry", data.get("db") == "global", f"got: {data.get('db')}")
        check("get global text", "bun" in data.get("text", ""), f"got: {data.get('text', '')[:50]}")

        # ── Test 5: get_memory from project ──
        print("\n[Test 5] get_memory (project entry)")
        resp = call_tool(proc, "get_memory", {"id": project_id})
        data = extract_text(resp)
        check("get project entry", data.get("db") == "project", f"got: {data.get('db')}")

        # ── Test 6: list_memories ──
        print("\n[Test 6] list_memories")
        resp = call_tool(proc, "list_memories", {"path_prefix": "/", "limit": 50})
        data = extract_text(resp)
        check("list returns list", isinstance(data, list), f"type: {type(data)}")
        if isinstance(data, list):
            dbs_seen = set(e.get("db") for e in data)
            check("list has both DBs", len(dbs_seen) >= 2, f"dbs: {dbs_seen}")

        # ── Test 7: memory_stats ──
        print("\n[Test 7] memory_stats")
        resp = call_tool(proc, "memory_stats")
        data = extract_text(resp)
        check("stats has total", "total" in data, f"keys: {list(data.keys()) if isinstance(data, dict) else data}")
        check("stats has databases", "databases" in data)
        if isinstance(data, dict) and "databases" in data:
            dbs = data["databases"]
            check("databases.global exists", "global" in dbs)
            check("databases.project exists", "project" in dbs)

        # ── Test 8: set_state / get_state (global only) ──
        print("\n[Test 8] set_state / get_state")
        resp = call_tool(proc, "set_state", {"key": "test_key", "value": {"hello": "world"}})
        data = extract_text(resp)
        check("set_state ok", data.get("key") == "test_key")

        resp = call_tool(proc, "get_state", {"key": "test_key"})
        data = extract_text(resp)
        check("get_state value", data.get("value") == {"hello": "world"}, f"got: {data}")

        # ── Test 9: get_pipeline_status ──
        print("\n[Test 9] get_pipeline_status")
        resp = call_tool(proc, "get_pipeline_status")
        data = extract_text(resp)
        check("pipeline status", data.get("status") == "running")
        check("pipeline has global_vec", "global_vec_available" in data or "vec_available" in str(data))

        # ── Test 10: hub_register (global) ──
        print("\n[Test 10] hub_register (global)")
        resp = call_tool(proc, "hub_register", {
            "id": "skill:code-review",
            "cap_type": "skill",
            "name": "Code Review",
            "description": "Review code diffs for bugs and style issues",
            "definition": '{"prompt": "Review this diff..."}',
            "scope": "global",
        })
        data = extract_text(resp)
        check("hub_register global", data.get("db") == "global", f"got: {data}")
        check("hub_register id", data.get("id") == "skill:code-review")

        # ── Test 11: hub_register (project) ──
        print("\n[Test 11] hub_register (project)")
        resp = call_tool(proc, "hub_register", {
            "id": "mcp:github",
            "cap_type": "mcp",
            "name": "GitHub MCP",
            "description": "GitHub API integration",
            "definition": '{"command": "npx", "args": ["gh-mcp-server"]}',
            "scope": "project",
        })
        data = extract_text(resp)
        check("hub_register project", data.get("db") == "project", f"got: {data}")

        # ── Test 12: hub_discover ──
        print("\n[Test 12] hub_discover (list all)")
        resp = call_tool(proc, "hub_discover", {"enabled_only": True})
        data = extract_text(resp)
        check("hub_discover returns list", isinstance(data, list), f"type: {type(data)}")
        if isinstance(data, list):
            ids = [c.get("id") for c in data]
            check("discover has skill", "skill:code-review" in ids, f"ids: {ids}")
            check("discover has mcp", "mcp:github" in ids, f"ids: {ids}")
            dbs = set(c.get("db") for c in data)
            check("discover has both dbs", len(dbs) >= 2, f"dbs: {dbs}")

        # ── Test 13: hub_discover (search) ──
        print("\n[Test 13] hub_discover (search)")
        resp = call_tool(proc, "hub_discover", {"query": "code review"})
        data = extract_text(resp)
        check("hub_search returns list", isinstance(data, list))
        if isinstance(data, list) and len(data) > 0:
            check("hub_search finds skill", data[0].get("id") == "skill:code-review")

        # ── Test 14: hub_get ──
        print("\n[Test 14] hub_get")
        resp = call_tool(proc, "hub_get", {"id": "skill:code-review"})
        data = extract_text(resp)
        check("hub_get found", data.get("id") == "skill:code-review", f"got: {data}")
        check("hub_get has definition", "prompt" in data.get("definition", ""))

        # ── Test 15: hub_feedback ──
        print("\n[Test 15] hub_feedback")
        resp = call_tool(proc, "hub_feedback", {
            "id": "skill:code-review",
            "success": True,
            "rating": 4.5,
        })
        data = extract_text(resp)
        check("hub_feedback recorded", data.get("recorded") == True, f"got: {data}")

        # verify metrics updated
        resp = call_tool(proc, "hub_get", {"id": "skill:code-review"})
        data = extract_text(resp)
        check("hub_feedback uses incremented", data.get("uses", 0) >= 1, f"uses: {data.get('uses')}")

        # ── Test 16: hub_stats ──
        print("\n[Test 16] hub_stats")
        resp = call_tool(proc, "hub_stats")
        data = extract_text(resp)
        check("hub_stats total", data.get("total_capabilities", 0) >= 2, f"got: {data}")
        check("hub_stats by_type", "skill" in data.get("by_type", {}), f"got: {data.get('by_type')}")

        # ── Summary ──
        # ── Test 17: hub_call (MCP proxy spike) ──
        print("\n[Test 17] hub_call (MCP proxy)")
        # First register the test echo MCP server
        import os as _os
        echo_server_path = _os.path.join(_os.path.dirname(__file__), "tests/test_echo_mcp_server.py")
        resp = call_tool(proc, "hub_register", {
            "id": "mcp:test-echo",
            "cap_type": "mcp",
            "name": "Test Echo Server",
            "description": "Test MCP server with echo and add tools",
            "definition": json.dumps({
                "transport": "stdio",
                "command": "python3",
                "args": [echo_server_path],
            }),
            "scope": "global",
        })
        data = extract_text(resp)
        check("register mcp server", data.get("id") == "mcp:test-echo", f"got: {data}")

        # Call echo tool through hub_call
        resp = call_tool(proc, "hub_call", {
            "server_id": "mcp:test-echo",
            "tool_name": "echo",
            "arguments": {"text": "hello from tachi"},
        })
        data = extract_text(resp)
        check("hub_call echo", "hello from tachi" in str(data.get("content", [])), f"got: {data}")
        check("hub_call not error", data.get("is_error") == False, f"got: {data}")

        # Call add tool through hub_call
        resp = call_tool(proc, "hub_call", {
            "server_id": "mcp:test-echo",
            "tool_name": "add",
            "arguments": {"a": 17, "b": 25},
        })
        data = extract_text(resp)
        check("hub_call add", "42" in str(data.get("content", [])), f"got: {data}")

        # ── Test 18: tools/list includes proxy tools ──
        print("\n[Test 18] tools/list includes proxy tools")
        resp = send_request(proc, {
            "jsonrpc": "2.0", "id": next_id(), "method": "tools/list",
            "params": {},
        })
        tool_names = []
        try:
            tool_names = [t["name"] for t in resp["result"]["tools"]]
        except (KeyError, TypeError):
            pass
        check("tools/list has native tools", "save_memory" in tool_names, f"tools: {tool_names[:5]}...")
        check("tools/list has proxy echo", "test-echo__echo" in tool_names, f"tools: {[t for t in tool_names if 'echo' in t]}")
        check("tools/list has proxy add", "test-echo__add" in tool_names, f"tools: {[t for t in tool_names if 'add' in t]}")

        # ── Test 19: call proxy tool directly (via transparent name) ──
        print("\n[Test 19] call proxy tool directly")
        resp = call_tool(proc, "test-echo__echo", {"text": "transparent proxy works"})
        data = extract_text(resp)
        check("proxy echo works", "transparent proxy works" in str(data), f"got: {data}")

        print("\n[Test 20] skill dynamic tool mapping")
        resp = call_tool(proc, "hub_register", {
            "id": "skill:smart-review",
            "cap_type": "skill",
            "name": "Smart Review",
            "description": "Run code review skill through Tachi dynamic tool",
            "definition": json.dumps({
                "prompt": "Review this code:\n{{input}}\n",
                "mock_response": "looks good",
                "inputSchema": {
                    "type": "object",
                    "properties": {"input": {"type": "string"}},
                    "required": ["input"]
                }
            }),
            "scope": "global",
        })
        data = extract_text(resp)
        check("register skill dynamic tool", data.get("id") == "skill:smart-review", f"got: {data}")
        check("register returns tool_name", data.get("tool_name") == "tachi_skill_smart_review", f"got: {data}")

        resp = send_request(proc, {
            "jsonrpc": "2.0", "id": next_id(), "method": "tools/list",
            "params": {},
        })
        skill_names = []
        try:
            skill_names = [t["name"] for t in resp["result"]["tools"]]
        except (KeyError, TypeError):
            pass
        check("tools/list has skill tool", "tachi_skill_smart_review" in skill_names, f"tools: {[n for n in skill_names if n.startswith('tachi_skill_')]}")

        resp = call_tool(proc, "tachi_skill_smart_review", {"input": "fn test() {}"})
        data = extract_text(resp)
        check("skill tool executes", data.get("output") == "looks good", f"got: {data}")

        print(f"\n{'='*40}")
        print(f"Results: {passed} passed, {failed} failed")
        return failed == 0

    except Exception as e:
        print(f"\nERROR: {e}")
        import traceback
        traceback.print_exc()
        return False
    finally:
        proc.terminate()
        proc.wait()
        # Print stderr for debugging
        stderr = proc.stderr.read().decode()
        if stderr:
            print(f"\n--- Server stderr ---\n{stderr}")
        # Cleanup
        shutil.rmtree(tmpdir, ignore_errors=True)

if __name__ == "__main__":
    success = test_memory_server()
    sys.exit(0 if success else 1)
