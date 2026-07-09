"""Duplicate-invoice detection tool for the AP-processing agent.

Bound to an ERP connection in config.yaml. A production implementation would
query the ERP's paid-invoice ledger at ERP_BASE_URL; for this self-contained
demo the tool falls back to a bundled mock ledger whenever the ERP is
unreachable (here: always), so apply and test succeed without a real ERP.
"""

import os

# Bundled mock ledger of invoices already seen in the AP system.
_SAMPLE_LEDGER = [
    {"invoice_number": "INV-5501", "vendor_id": "V100", "amount": 4250.00, "invoice_date": "2024-05-02"},
    {"invoice_number": "INV-5502", "vendor_id": "V101", "amount": 12750.00, "invoice_date": "2024-05-03"},
    {"invoice_number": "INV-5510", "vendor_id": "V102", "amount": 600.00, "invoice_date": "2024-05-06"},
]


def _lookup_ledger() -> list:
    """Return the invoice ledger to check against.

    A real implementation would fetch the ERP ledger from ERP_BASE_URL using
    the bound connection's credentials; this demo always returns the bundled
    mock ledger so it never depends on a reachable ERP.
    """
    _ = os.environ.get("ERP_BASE_URL")  # present only to show where the ERP would be read
    return _SAMPLE_LEDGER


def main(invoice_number: str, vendor_id: str, amount: float, invoice_date: str) -> dict:
    """Detect whether an invoice duplicates one already in the ledger."""
    vendor_id = vendor_id.strip().upper()
    invoice_number = invoice_number.strip().upper()
    invoice_amount = float(amount)
    ledger = _lookup_ledger()

    for seen in ledger:
        same_number = seen["vendor_id"] == vendor_id and seen["invoice_number"] == invoice_number
        same_triple = (
            seen["vendor_id"] == vendor_id
            and abs(float(seen["amount"]) - invoice_amount) < 0.01
            and seen["invoice_date"] == invoice_date.strip()
        )
        if same_number or same_triple:
            reason = "same vendor and invoice number" if same_number else "same vendor, amount, and date"
            return {
                "duplicate": True,
                "reason": reason,
                "matched_invoice": seen["invoice_number"],
                "source": "demo_erp_ledger",
            }

    return {
        "duplicate": False,
        "reason": "no matching invoice found in the ledger",
        "matched_invoice": None,
        "source": "demo_erp_ledger",
    }
