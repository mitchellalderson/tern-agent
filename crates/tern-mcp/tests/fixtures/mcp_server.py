#!/usr/bin/env python3
"""Minimal MCP server for tests (stdlib only, independent of rmcp).

    mcp_server.py                 stdio transport
    mcp_server.py --http          streamable HTTP on 127.0.0.1; prints the port

Tools: echo(text), add(a, b), fail(), env(name), crash() (exits the process).
Every tools/call is appended to $MCP_FIXTURE_LOG if set, one name per line.
"""

import json
import os
import sys

PROTOCOL = "2025-06-18"

TOOLS = [
    {"name": "echo", "description": "Echo the text back", "annotations": {"readOnlyHint": True},
     "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}},
    {"name": "add", "description": "Add two numbers",
     "inputSchema": {"type": "object", "properties": {"a": {"type": "number"}, "b": {"type": "number"}},
                     "required": ["a", "b"]}},
    {"name": "fail", "description": "Always reports an error", "inputSchema": {"type": "object"},
     "annotations": {"readOnlyHint": False, "destructiveHint": True}},
    {"name": "env", "description": "Read an environment variable",
     "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}},
    {"name": "crash", "description": "Exit the server process", "inputSchema": {"type": "object"},
     "annotations": {"destructiveHint": False}},
]


def text(s, error=False):
    return {"content": [{"type": "text", "text": s}], "isError": error}


def call_tool(name, args):
    log = os.environ.get("MCP_FIXTURE_LOG")
    if log:
        with open(log, "a") as f:
            f.write(name + "\n")
    if name == "echo":
        return text(args.get("text", ""))
    if name == "add":
        return text(str(args["a"] + args["b"]))
    if name == "fail":
        return text("something went wrong", error=True)
    if name == "env":
        return text(os.environ.get(args["name"], "<unset>"))
    if name == "crash":
        sys.stdout.flush()
        os._exit(3)
    return None


def handle(msg):
    """Return a response dict, or None for notifications."""
    if "id" not in msg:
        return None
    method, params, rid = msg.get("method"), msg.get("params") or {}, msg["id"]
    if method == "initialize":
        result = {"protocolVersion": PROTOCOL, "capabilities": {"tools": {}},
                  "serverInfo": {"name": "fixture", "version": "1.0"}}
    elif method == "tools/list":
        result = {"tools": TOOLS}
    elif method == "tools/call":
        result = call_tool(params.get("name"), params.get("arguments") or {})
        if result is None:
            return {"jsonrpc": "2.0", "id": rid, "error": {"code": -32602, "message": "unknown tool"}}
    elif method == "ping":
        result = {}
    else:
        return {"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": "method not found"}}
    return {"jsonrpc": "2.0", "id": rid, "result": result}


def stdio():
    print("fixture server starting", file=sys.stderr, flush=True)
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        resp = handle(json.loads(line))
        if resp is not None:
            sys.stdout.write(json.dumps(resp) + "\n")
            sys.stdout.flush()


def http():
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
            msg = json.loads(body)
            resp = handle(msg)
            if resp is None:
                self.send_response(202)
                self.end_headers()
                return
            data = json.dumps(resp).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            if msg.get("method") == "initialize":
                self.send_header("Mcp-Session-Id", "fixture-session")
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):
            self.send_response(405)
            self.end_headers()

        def do_DELETE(self):
            self.send_response(200)
            self.end_headers()

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    print(server.server_address[1], flush=True)
    server.serve_forever()


if __name__ == "__main__":
    http() if "--http" in sys.argv else stdio()
