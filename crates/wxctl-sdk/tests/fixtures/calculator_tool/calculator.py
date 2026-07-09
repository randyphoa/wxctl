def main(operation: str, a: float, b: float) -> dict:
    if operation == "add":
        result = a + b
    elif operation == "multiply":
        result = a * b
    else:
        return {"error": f"Unknown operation: {operation}"}
    return {"operation": operation, "a": a, "b": b, "result": result}
