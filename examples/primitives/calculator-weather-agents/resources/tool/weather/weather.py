"""
Weather forecast tool for watsonx Orchestrate
Returns mocked weather forecast data for a given city
"""


def main(city: str) -> dict:
    """
    Main entry point for the tool

    Args:
        city: The name of the city to get the weather forecast for

    Returns:
        Result dictionary with weather forecast details
    """
    forecasts = {
        "new york": {
            "city": "New York",
            "temperature_f": 35,
            "condition": "Partly Cloudy",
            "humidity": 55,
            "wind_mph": 12,
            "forecast": "Cold with partly cloudy skies. Expect temperatures around 35°F with moderate winds.",
        },
        "london": {
            "city": "London",
            "temperature_f": 45,
            "condition": "Rainy",
            "humidity": 80,
            "wind_mph": 8,
            "forecast": "Overcast with light rain throughout the day. Temperatures around 45°F.",
        },
        "tokyo": {
            "city": "Tokyo",
            "temperature_f": 50,
            "condition": "Sunny",
            "humidity": 40,
            "wind_mph": 5,
            "forecast": "Clear skies and sunny. Pleasant temperatures around 50°F with light winds.",
        },
        "sydney": {
            "city": "Sydney",
            "temperature_f": 78,
            "condition": "Sunny",
            "humidity": 60,
            "wind_mph": 10,
            "forecast": "Warm and sunny. Temperatures around 78°F with a gentle breeze.",
        },
    }

    lookup = city.strip().lower()
    if lookup in forecasts:
        return forecasts[lookup]

    return {
        "city": city,
        "temperature_f": 65,
        "condition": "Clear",
        "humidity": 50,
        "wind_mph": 7,
        "forecast": f"Fair weather expected in {city}. Temperatures around 65°F with calm winds.",
    }
