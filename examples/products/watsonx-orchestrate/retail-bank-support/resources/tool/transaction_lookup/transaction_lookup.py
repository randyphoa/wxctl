"""Recent-transaction lookup tool for the retail-bank support agent.

Serves recent transactions from a bundled sample directory, so the tool
always returns well-formed data without an external core-banking dependency.
"""

# Sample recent transactions per account (mock core-banking data).
_SAMPLE_TRANSACTIONS = {
    "ACC-1001": [
        {"date": "2024-05-01", "description": "Payroll deposit", "amount": 2100.00, "type": "credit"},
        {"date": "2024-05-03", "description": "Grocery store", "amount": 84.20, "type": "debit"},
        {"date": "2024-05-05", "description": "Monthly maintenance fee", "amount": 12.00, "type": "debit"},
    ],
    "ACC-1002": [
        {"date": "2024-05-02", "description": "Interest credit", "amount": 31.45, "type": "credit"},
        {"date": "2024-05-04", "description": "Transfer to checking", "amount": 500.00, "type": "debit"},
    ],
    "ACC-1003": [
        {"date": "2024-04-28", "description": "Overdraft fee", "amount": 35.00, "type": "debit"},
    ],
}


def main(account_id: str) -> dict:
    """Return recent transactions for an account from the bundled sample data."""
    account_id = account_id.strip().upper()
    txns = _SAMPLE_TRANSACTIONS.get(account_id)
    if txns is None:
        return {
            "found": False,
            "account_id": account_id,
            "transactions": [],
            "source": "demo_core_banking",
            "message": f"No transactions found for {account_id}.",
        }
    return {
        "found": True,
        "account_id": account_id,
        "transactions": txns,
        "source": "demo_core_banking",
    }
