"""score_applicant — score a loan applicant against the governed watsonx.ai deployment.

This is the cross-product seam: a watsonx Orchestrate tool that calls the *same*
credit-risk deployment a watsonx.governance OpenScale quality monitor is bound to.
The agent never re-implements the scoring logic — it invokes the live, governed
model, so every score the assistant gives is the one OpenScale is watching.

Credentials reach the tool through a watsonx Orchestrate **connection** (the
`crg-scorer` key_value connection), whose values wxctl wires at apply time:
`wml_url`, `apikey`, and the watsonx.ai `deployment_id`. We read them via the
supported runtime connections API and fall back to the documented
`WXO_CONNECTION_{app_id}_{key}` environment contract the runtime injects.
"""

import json
import urllib.error
import urllib.parse
import urllib.request

# Matches the connection's app_id in config.yaml and the binding.python.connections key.
APP_ID = "crg-scorer"
# NB: the endpoint key is `wml_url`, not `url` — `url` is a reserved key in a
# watsonx Orchestrate connection (treated as the connection's server_url) and is
# stripped from the runtime credentials, so it never reaches the tool.
_WANTED = ("wml_url", "apikey", "deployment_id")


def _load_credentials():
    """Read the scorer connection's key-value credentials at runtime.

    Preferred path is the supported watsonx Orchestrate runtime API; the fallback
    reads the `WXO_CONNECTION_{app_id}_{key}` env vars the runtime injects for every
    connection a tool declares (the same vars the SDK itself reads).
    """
    try:
        from ibm_watsonx_orchestrate.run import connections

        kv = connections.key_value(APP_ID)
        return {key: kv.get(key) for key in _WANTED}
    except Exception:
        import os

        found = {}
        for env_key, value in os.environ.items():
            if not env_key.startswith("WXO_CONNECTION_"):
                continue
            for key in _WANTED:
                if env_key.endswith("_" + key):
                    found[key] = value
        return found


def _iam_token(apikey):
    """Exchange an IBM Cloud API key for a bearer access token."""
    data = urllib.parse.urlencode({"grant_type": "urn:ibm:params:oauth:grant-type:apikey", "apikey": apikey}).encode()
    req = urllib.request.Request(
        "https://iam.cloud.ibm.com/identity/token",
        data=data,
        headers={"Content-Type": "application/x-www-form-urlencoded", "Accept": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.load(resp)["access_token"]


def main(annual_income, debt_to_income, credit_score):
    """Score one loan applicant against the governed watsonx.ai deployment.

    Returns the live model's approve/decline decision and probability — the same
    output the watsonx.governance OpenScale quality monitor is subscribed to.
    """
    creds = _load_credentials()
    missing = [key for key in ("wml_url", "apikey", "deployment_id") if not creds.get(key)]
    if missing:
        return {"error": f"scorer connection is missing required values: {', '.join(missing)}"}

    base_url = (creds.get("wml_url") or "").rstrip("/")
    deployment_id = creds["deployment_id"]
    payload = {
        "input_data": [
            {
                "fields": ["annual_income", "debt_to_income", "credit_score"],
                "values": [[annual_income, debt_to_income, credit_score]],
            }
        ]
    }

    try:
        token = _iam_token(creds["apikey"])
        endpoint = f"{base_url}/ml/v4/deployments/{deployment_id}/predictions?version=2021-05-01"
        req = urllib.request.Request(
            endpoint,
            data=json.dumps(payload).encode(),
            headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
            method="POST",
        )
        with urllib.request.urlopen(req, timeout=60) as resp:
            result = json.load(resp)
    except urllib.error.HTTPError as exc:
        return {"error": f"scoring call failed: HTTP {exc.code}", "detail": exc.read().decode("utf-8", "replace")[:500]}
    except Exception as exc:  # noqa: BLE001 — surface any transport error to the agent
        return {"error": f"scoring call failed: {exc}"}

    decision, probability = result["predictions"][0]["values"][0][:2]
    return {
        "decision": "approve" if decision == 1 else "decline",
        "approved": decision == 1,
        "probability": probability,
        "annual_income": annual_income,
        "debt_to_income": debt_to_income,
        "credit_score": credit_score,
        "scored_by": "watsonx.ai deployment governed by a watsonx.governance OpenScale quality monitor",
    }
