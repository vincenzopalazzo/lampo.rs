import json
import socket


class LampoClient:
    """
    A simple Lampo client that communicates via a Unix socket.
    """

    def __init__(self, socket_path: str):
        """
        Initializes the LampoClient instance.

        Args:
          socket_path: The path to the Lampo socket.
        """

        self.socket_path = socket_path

    def call(self, method: str, params: dict = None) -> dict:
        """
        Calls a method on the Lampo client over the Unix socket.

        Args:
          method: The name of the Lampo method to call.
          params: (Optional) A dictionary of parameters to pass to the method.

        Returns:
          The response from the Lampo client as a dictionary.

        Raises:
          Exception: If there is an error communicating with the Lampo client.
        """

        request = {
            "method": method,
            "params": params if params else {},
            "id": "pylampo-client/1",
            "jsonrpc": "2.0",
        }
        try:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
                sock.connect(self.socket_path)
                sock.sendall(json.dumps(request).encode())

                response = ""
                while True:
                    data = sock.recv(1024)
                    if not data:
                        break
                    response += data.decode()

                return json.loads(response)
        except Exception as e:
            raise Exception(f"Error communicating with Lampo client: {e}")
