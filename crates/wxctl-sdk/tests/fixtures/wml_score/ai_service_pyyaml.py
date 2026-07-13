def deployable_ai_service(context):
    """AI Service that verifies the 'pyyaml' package is installed."""

    def generate(context) -> dict:
        import yaml
        return {"body": {"package": "pyyaml", "version": yaml.__version__}}

    def generate_stream(context):
        yield generate(context)

    return generate, generate_stream
