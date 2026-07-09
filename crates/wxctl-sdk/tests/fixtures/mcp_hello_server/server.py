"""Minimal MCP hello world server for integration testing.

This server exposes a single 'hello' tool that returns a greeting.
Used by wxctl live tests to verify the artifact upload flow
(source: files → ZIP → POST /v1/toolkits/{id}/upload).
"""

import json
import sys


def handle_request(request):
    method = request.get("method", "")

    if method == "initialize":
        return {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {"listChanged": False}},
            "serverInfo": {"name": "hello-server", "version": "1.0.0"},
        }

    if method == "tools/list":
        return {
            "tools": [
                {
                    "name": "hello",
                    "description": "Returns a greeting",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "name": {"type": "string", "description": "Name to greet"}
                        },
                        "required": ["name"],
                    },
                }
            ]
        }

    if method == "tools/call":
        name = request.get("params", {}).get("arguments", {}).get("name", "world")
        return {"content": [{"type": "text", "text": f"Hello, {name}!"}]}

    return {"error": {"code": -32601, "message": f"Unknown method: {method}"}}


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        request = json.loads(line)
        response = {"jsonrpc": "2.0", "id": request.get("id")}
        response.update({"result": handle_request(request)})
        sys.stdout.write(json.dumps(response) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
