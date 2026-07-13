from ibm_watsonx_orchestrate.run import connections


def main(query: str) -> dict:
    """Look up information using the connected service.

    Retrieves credentials from the configured basic_auth connection
    and returns canned results. The connection wiring is the thing
    being tested, not the external service itself.
    """
    creds = connections.basic_auth("wxctl_test_service")

    return {
        "status": "ok",
        "connected": True,
        "query": query,
        "results": [
            {"name": "Acme Corp", "id": "ACM-001"},
            {"name": "Globex Inc", "id": "GLX-002"},
        ],
    }
