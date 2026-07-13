"""
Calculator tool for watsonx Orchestrate
Supports add, multiply, and divide operations
"""


def add(a: float, b: float) -> float:
    """
    Add two numbers together

    Args:
        a: First number
        b: Second number

    Returns:
        Sum of a and b
    """
    return a + b


def multiply(a: float, b: float) -> float:
    """
    Multiply two numbers

    Args:
        a: First number
        b: Second number

    Returns:
        Product of a and b
    """
    return a * b


def divide(a: float, b: float) -> float:
    """
    Divide two numbers

    Args:
        a: First number (dividend)
        b: Second number (divisor)

    Returns:
        Quotient of a divided by b
    """
    if b == 0:
        raise ValueError("Cannot divide by zero")
    return a / b


def main(operation: str, a: float, b: float) -> dict:
    """
    Main entry point for the tool

    Args:
        operation: The mathematical operation to perform ('add', 'multiply', or 'divide')
        a: The first numeric operand to use in the calculation
        b: The second numeric operand to use in the calculation

    Returns:
        Result dictionary with operation details and calculated result
    """
    if operation == "add":
        result = add(a, b)
    elif operation == "multiply":
        result = multiply(a, b)
    elif operation == "divide":
        try:
            result = divide(a, b)
        except ValueError as e:
            return {"error": str(e)}
    else:
        return {"error": f"Unknown operation: {operation}"}

    return {
        "operation": operation,
        "a": a,
        "b": b,
        "result": result
    }
