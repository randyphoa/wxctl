"""Minimal ops MCP server using FastMCP.

Exposes a single 'service_status' tool that returns the current status of a
named service from bundled mock data — no network, no external service.
"""

from fastmcp import FastMCP

mcp = FastMCP("ops-server")

# Canned service status keyed by lowercased service name.
_STATUS = {
    "billing": {"service": "billing", "status": "operational", "uptime": "99.98%", "region": "us-east"},
    "auth": {"service": "auth", "status": "degraded", "uptime": "99.40%", "region": "us-east"},
    "search": {"service": "search", "status": "operational", "uptime": "99.95%", "region": "eu-west"},
}


@mcp.tool
def service_status(service: str) -> dict:
    """Returns the current status of a named service from bundled mock data."""
    key = (service or "").strip().lower()
    record = _STATUS.get(key)
    if record is None:
        return {"service": service, "status": "unknown", "uptime": "n/a", "region": "n/a"}
    return record


if __name__ == "__main__":
    mcp.run()
