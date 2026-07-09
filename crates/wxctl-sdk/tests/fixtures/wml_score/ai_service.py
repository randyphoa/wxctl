def deployable_ai_service(context):
    """AI Service entry point — returns (generate, generate_stream) tuple."""

    def generate(context) -> dict:
        payload = context.get_json()
        return {"body": payload}

    def generate_stream(context):
        yield generate(context)

    return generate, generate_stream
