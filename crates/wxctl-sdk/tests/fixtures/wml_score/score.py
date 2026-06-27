def score(payload):
    """Minimal scoring function for live tests."""
    input_data = payload.get("input_data", [{}])
    values = input_data[0].get("values", []) if input_data else []
    return {"predictions": [{"values": values}]}
