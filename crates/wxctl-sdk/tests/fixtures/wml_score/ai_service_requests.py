def deployable_ai_service(context):
    """AI Service that verifies the 'requests' package is installed."""

    def generate(context) -> dict:
        import requests
        return {"body": {"package": "requests", "version": requests.__version__}}

    def generate_stream(context):
        yield generate(context)

    return generate, generate_stream
