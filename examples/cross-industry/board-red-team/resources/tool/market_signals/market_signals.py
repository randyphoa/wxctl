"""market_signals — mock market-context lookup for the board red-team capstone.

Returns canned market signals (sentiment, momentum, notable headlines) for a
named company. Bundled sample data only — no network, no external service.
"""

# Canned market context keyed by lowercased company name.
_SIGNALS = {
    "northwind": {
        "company": "Northwind Industries",
        "sentiment": "cautiously positive",
        "momentum": "flat revenue, margin compression in core segment",
        "headlines": [
            "Analysts flag rising input costs",
            "New entrant undercutting on price in the mid-market",
        ],
        "regulatory_watch": "pending data-privacy ruling in primary market",
    },
    "meridian": {
        "company": "Meridian Corp",
        "sentiment": "negative",
        "momentum": "two consecutive quarters of declining bookings",
        "headlines": [
            "Activist fund discloses 6% stake",
            "Credit outlook revised to negative",
        ],
        "regulatory_watch": "antitrust review of proposed acquisition",
    },
    "atlas": {
        "company": "Atlas Holdings",
        "sentiment": "positive",
        "momentum": "double-digit growth in new product line",
        "headlines": [
            "Beat consensus on revenue and EPS",
            "Expanding into adjacent vertical",
        ],
        "regulatory_watch": "none material",
    },
}


def main(company):
    """Return canned market signals for a company name (case-insensitive)."""
    key = (company or "").strip().lower()
    record = _SIGNALS.get(key)
    if record is None:
        return {
            "company": company,
            "sentiment": "unknown",
            "momentum": "no market signal on file for this company",
            "headlines": [],
            "regulatory_watch": "unknown",
        }
    return record
