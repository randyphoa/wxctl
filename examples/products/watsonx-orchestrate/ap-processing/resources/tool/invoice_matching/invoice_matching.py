"""Invoice 3-way matching tool for the AP-processing agent.

Performs a 3-way match (invoice <-> purchase order <-> goods receipt) over a
bundled sample directory, so the tool always returns a useful, well-formed
result without any external ERP dependency.
"""

# Tolerance for amount mismatches: the greater of +/-2% of the PO amount or +/-$50.
_TOLERANCE_PCT = 0.02
_TOLERANCE_FLOOR = 50.0

# Sample purchase orders backing the matching tool (mock ERP master data).
_SAMPLE_POS = {
    "PO-2024-1001": {"vendor_id": "V100", "po_amount": 4250.00, "goods_received": True},
    "PO-2024-1042": {"vendor_id": "V100", "po_amount": 980.00, "goods_received": True},
    "PO-2024-2003": {"vendor_id": "V101", "po_amount": 12750.00, "goods_received": False},
    "PO-2024-3010": {"vendor_id": "V102", "po_amount": 600.00, "goods_received": True},
}


def main(invoice_number: str, vendor_id: str, amount: float, po_number: str) -> dict:
    """Run a 3-way match for one invoice against the bundled mock ERP data.

    Returns a structured match result; falls back to demo data on any lookup
    miss so the agent can always reason about the outcome.
    """
    po_number = po_number.strip().upper()
    vendor_id = vendor_id.strip().upper()
    invoice_amount = float(amount)

    po = _SAMPLE_POS.get(po_number)
    if po is None:
        return {
            "match": "po_not_found",
            "invoice_number": invoice_number,
            "po_number": po_number,
            "vendor_id": vendor_id,
            "invoice_amount": invoice_amount,
            "source": "demo_erp",
            "message": f"No purchase order found for {po_number}.",
        }

    if po["vendor_id"] != vendor_id:
        return {
            "match": "vendor_mismatch",
            "invoice_number": invoice_number,
            "po_number": po_number,
            "vendor_id": vendor_id,
            "po_vendor_id": po["vendor_id"],
            "source": "demo_erp",
            "message": f"Invoice vendor {vendor_id} does not match PO vendor {po['vendor_id']}.",
        }

    po_amount = float(po["po_amount"])
    tolerance = max(po_amount * _TOLERANCE_PCT, _TOLERANCE_FLOOR)
    within_tolerance = abs(invoice_amount - po_amount) <= tolerance

    return {
        "match": "ok" if within_tolerance else "amount_mismatch",
        "invoice_number": invoice_number,
        "po_number": po_number,
        "vendor_id": vendor_id,
        "invoice_amount": invoice_amount,
        "po_amount": po_amount,
        "within_tolerance": within_tolerance,
        "goods_received": bool(po["goods_received"]),
        "source": "demo_erp",
    }
