import requests


class LampoClient:
    """
    A simple Lampo client that communicates via HTTP.
    """

    def __init__(self, base_url: str):
        """
        Initializes the LampoClient instance.

        Args:
          base_url: The base URL of the Lampo server.
        """

        self.base_url = base_url

    def call(self, method: str, params: dict = None) -> dict:
        """
        Calls a method on the Lampo client over HTTP.

        Args:
          method: The name of the Lampo method to call.
          params: (Optional) A dictionary of parameters to pass to the method.

        Returns:
          The response from the Lampo client as a dictionary.

        Raises:
          Exception: If there is an error communicating with the Lampo client.
        """
        import logging

        url = f"{self.base_url}/{method}"
        request = params
        logging.debug(f"Calling Lampo client at {url} with request: {request}")
        try:
            headers = {"accept": "application/json"}
            response = requests.post(url, json=request, headers=headers)
            response.raise_for_status()
            return response.json()
        except requests.exceptions.RequestException as e:
            error_message = e.response.text if e.response else str(e)
            raise Exception(f"Error communicating with Lampo client: {error_message}")
