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

        url = f"{self.base_url}/{method}"
        request = {
            "params": params if params else {},
            "id": "pylampo-client/1",
            "jsonrpc": "2.0",
        }
        try:
            headers = {"accept": "application/json"}
            response = requests.post(url, json=request, headers=headers)
            response.raise_for_status()
            return response
        except Exception as e:
            raise Exception(f"Error communicating with Lampo client: {e}")
