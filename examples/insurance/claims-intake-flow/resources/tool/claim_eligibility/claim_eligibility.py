"""claim_eligibility — validate a claim ID and report member eligibility.

Bundled sample data only — no network, no external service. Returns whether a
claim ID is well-formed and the member's eligibility status for processing.
"""

# Canned member eligibility keyed by claim ID.
_CLAIMS = {
    "CLM-1001": {"member": "M-204", "status": "eligible", "plan": "PPO-Gold"},
    "CLM-1002": {"member": "M-318", "status": "pending-docs", "plan": "HMO-Silver"},
    "CLM-1003": {"member": "M-477", "status": "ineligible", "plan": "lapsed"},
}


def main(claim_id):
    """Validate a claim ID (format CLM-NNNN) and return member eligibility."""
    cid = (claim_id or "").strip().upper()
    if not cid.startswith("CLM-") or not cid[4:].isdigit():
        return {"claim_id": claim_id, "valid": False, "reason": "claim ID must look like CLM-1001"}
    record = _CLAIMS.get(cid)
    if record is None:
        return {"claim_id": cid, "valid": True, "found": False, "status": "unknown"}
    return {
        "claim_id": cid,
        "valid": True,
        "found": True,
        "member": record["member"],
        "status": record["status"],
        "plan": record["plan"],
    }
