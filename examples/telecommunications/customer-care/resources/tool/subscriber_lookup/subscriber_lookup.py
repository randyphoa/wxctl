"""Subscriber lookup tool for the telecom customer-care agent.

Serves subscriber plan and data-usage records from a bundled sample
directory keyed by mobile number (MSISDN). All demonstration numbers use
the reserved 555 range. The tool returns plan and usage only — never stored
personal details beyond the MSISDN the caller supplied.
"""

# Sample subscriber directory. MSISDNs use the reserved 555 range; records
# carry plan and usage only — no name, address, or payment details.
_SAMPLE_SUBSCRIBERS = {
    "+1-555-0142": {
        "msisdn": "+1-555-0142",
        "plan": "Essential",
        "data_allowance_gb": 10,
        "data_used_gb": 9.6,
        "throttled": True,
        "roaming_enabled": False,
        "status": "Active",
    },
    "+1-555-0173": {
        "msisdn": "+1-555-0173",
        "plan": "Plus",
        "data_allowance_gb": 50,
        "data_used_gb": 18.2,
        "throttled": False,
        "roaming_enabled": True,
        "status": "Active",
    },
    "+1-555-0188": {
        "msisdn": "+1-555-0188",
        "plan": "Unlimited",
        "data_allowance_gb": None,
        "data_used_gb": 63.4,
        "throttled": False,
        "roaming_enabled": False,
        "status": "Active",
    },
}


def _normalize(msisdn: str) -> str:
    """Normalize a supplied number to the +1-555-01xx sample form."""
    digits = "".join(ch for ch in msisdn if ch.isdigit())
    if len(digits) == 10:
        digits = "1" + digits
    if len(digits) == 11 and digits.startswith("1"):
        return f"+1-{digits[1:4]}-{digits[4:8]}"
    return msisdn.strip()


def main(msisdn: str) -> dict:
    """Look up a subscriber's plan and data usage by mobile number (MSISDN).

    Returns the plan name, data allowance and usage, throttle and roaming
    status from the bundled sample directory. Returns plan and usage only;
    never stored personal details beyond the supplied MSISDN.
    """
    key = _normalize(msisdn)

    record = _SAMPLE_SUBSCRIBERS.get(key)
    if record is None:
        return {
            "found": False,
            "msisdn": key,
            "source": "demo_subscribers",
            "message": f"No subscriber found for number {key}.",
        }
    return {"found": True, "source": "demo_subscribers", **record}
