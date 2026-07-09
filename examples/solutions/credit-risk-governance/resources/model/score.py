def score(payload):
    """Deterministic, transparent credit-risk scorer — no training, no model
    load, no randomness, so the deployment's predictions are exact and the
    kind: test assertion is stable.

    Each input feature row is [annual_income, debt_to_income, credit_score].
    Each output row is [decision, probability]:
      decision = 1 (approve) iff credit_score >= 650 AND debt_to_income <= 0.40,
                 else 0 (decline)
      probability = round(min(credit_score, 850) / 850, 3)

    WML requires each predictions[] item to be a dict ({"values": ...}).
    """
    rows = []
    for record in payload.get("input_data", []):
        for values in record.get("values", []):
            _annual_income, debt_to_income, credit_score = values
            decision = 1 if (credit_score >= 650 and debt_to_income <= 0.40) else 0
            probability = round(min(credit_score, 850) / 850, 3)
            rows.append([decision, probability])
    return {"predictions": [{"values": rows}]}
