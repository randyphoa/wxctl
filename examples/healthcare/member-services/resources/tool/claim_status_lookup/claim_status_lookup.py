"""Claim status lookup tool for the member-services agent.

Serves claim records from a bundled sample directory, so the tool always
returns a well-formed claim status without any external claims-system
dependency. The tool reports status only; it does not make coverage or
eligibility determinations.
"""

# Sample claims directory backing the lookup tool. Fictional members and
# claim IDs only — no real PII.
_SAMPLE_CLAIMS = {
    "CLM1001": {
        "claim_id": "CLM1001",
        "member_id": "M001",
        "date_of_service": "2026-03-04",
        "provider": "Lakeside Primary Care",
        "billed_amount": 220.00,
        "allowed_amount": 160.00,
        "plan_paid": 135.00,
        "member_responsibility": 25.00,
        "status": "Paid",
    },
    "CLM1002": {
        "claim_id": "CLM1002",
        "member_id": "M002",
        "date_of_service": "2026-04-18",
        "provider": "Northgate Imaging Center",
        "billed_amount": 1200.00,
        "allowed_amount": 800.00,
        "plan_paid": 0.00,
        "member_responsibility": 0.00,
        "status": "In Review",
    },
    "CLM1003": {
        "claim_id": "CLM1003",
        "member_id": "M003",
        "date_of_service": "2026-02-09",
        "provider": "Harborview Specialists",
        "billed_amount": 540.00,
        "allowed_amount": 0.00,
        "plan_paid": 0.00,
        "member_responsibility": 0.00,
        "status": "Denied",
    },
}


def main(claim_id: str) -> dict:
    """Look up the status of a submitted claim by claim ID.

    Returns the claim's status (Received, In Review, Approved, Denied, or
    Paid) plus the billed/allowed/paid amounts and member responsibility
    from the bundled sample directory. Reports status only; it does not make
    coverage or eligibility determinations.
    """
    claim_id = claim_id.strip().upper()

    record = _SAMPLE_CLAIMS.get(claim_id)
    if record is None:
        return {
            "found": False,
            "claim_id": claim_id,
            "source": "demo_claims",
            "message": f"No claim found for ID {claim_id}.",
        }
    return {"found": True, "source": "demo_claims", **record}
