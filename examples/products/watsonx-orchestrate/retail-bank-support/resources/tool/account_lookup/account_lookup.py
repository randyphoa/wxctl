"""Account lookup tool for the retail-bank support agent.

Serves account records from a bundled sample directory, so the tool always
returns a useful, well-formed record without any external core-banking
dependency. Returns product and balance information only -- no PII.
"""

# Sample account directory backing the lookup tool (mock core-banking data).
_SAMPLE_ACCOUNTS = {
    "ACC-1001": {
        "account_id": "ACC-1001",
        "account_type": "checking",
        "product_name": "Riverstone Everyday Checking",
        "balance": 2480.55,
        "status": "active",
        "opened_date": "2022-03-14",
    },
    "ACC-1002": {
        "account_id": "ACC-1002",
        "account_type": "savings",
        "product_name": "Riverstone High-Yield Savings",
        "balance": 15230.00,
        "status": "active",
        "opened_date": "2021-11-02",
    },
    "ACC-1003": {
        "account_id": "ACC-1003",
        "account_type": "money_market",
        "product_name": "Riverstone Money Market",
        "balance": 540.10,
        "status": "frozen",
        "opened_date": "2023-07-19",
    },
}


def main(account_id: str) -> dict:
    """Look up a bank account by account ID from the bundled sample directory."""
    account_id = account_id.strip().upper()
    record = _SAMPLE_ACCOUNTS.get(account_id)
    if record is None:
        return {
            "found": False,
            "account_id": account_id,
            "source": "demo_core_banking",
            "message": f"No account found for {account_id}.",
        }
    return {"found": True, "source": "demo_core_banking", **record}
