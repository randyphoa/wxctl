def score(payload):
    """Python script entry point — accepts payload, returns predictions."""
    input_data = payload.get("input_data", [{}])
    values = input_data[0].get("values", []) if input_data else []
    return {"predictions": [{"values": values}]}
